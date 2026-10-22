//! Expression checking: literals, identifiers, `this`, member and element access, object
//! and array literals, operators, assignments, conditionals and assertions.
//!
//! `check_expr` caches results per node while not checking speculatively. A node checked
//! quietly (while computing a declaration's type) is checked again, reporting, when the
//! per-file walk reaches it; the `checked` set makes that happen once.

use ts_r_ast::{
    AssignOp, BinaryOp, KeywordTypeKind, MethodKind, Node, NodeId, Span, UnaryOp, UpdateOp,
};
use ts_r_binder::{SymbolFacts, SymbolFlags, meaning};
use ts_r_types::{
    AliasId, IndexInfo, ObjectFlags, RelationKind, ResolvedMembers, StrId, SymId, TypeFlags,
    TypeId, TypeKind, TypeTraits,
};

use crate::checker::{Ck, GlobalType, Loc, MAX_FILE_SPECULATIVE_WORK, MAX_SPECULATIVE_WORK};
use crate::declared::{TupleEl, bigint_text};
use crate::diag::{Chain, dg};
use crate::nodes::{entity_text, key_text, skip_parens};
use crate::relate::Head;
use crate::symbols::{SynthSym, check_flags};
use crate::unions::Reduction;

const MAX_EXPR_DEPTH: u32 = 400;

/// Truthiness facts of a type: whether a value can be truthy and whether it can be falsy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Truthiness {
    pub truthy: bool,
    pub falsy: bool,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The type of an expression (contextually typed by its position).
    pub fn check_expr(&mut self, e: Loc) -> TypeId {
        let plain = self.speculative == 0 && self.context_overrides.is_empty();
        let cacheable = plain && self.flow_loop == 0;
        let loop_cacheable = plain && self.flow_loop > 0;
        let reporting = self.reporting(e.file);
        if (cacheable || loop_cacheable) && (!reporting || self.checked.contains(&e)) {
            let hit = if cacheable {
                self.node_types.get(&e).copied()
            } else {
                self.loop_node_types.get(&e).map(|v| v.0)
            };
            if let Some(t) = hit {
                return t;
            }
        }
        // While speculating nothing is published, but under unchanged assumptions an
        // expression has one type: flow narrowing re-checks the operands of every condition
        // on the way back, per reference, which multiplies without this.
        let spec_cacheable = self.speculative > 0 && self.flow_loop == 0;
        if spec_cacheable
            && let Some(&(epoch, t)) = self.spec_node_types.get(&e)
            && epoch == self.spec_epoch
        {
            return t;
        }
        if self.depth >= MAX_EXPR_DEPTH {
            return TypeId::UNRESOLVED;
        }
        if self.speculative > 0 {
            if self.spec_work >= MAX_SPECULATIVE_WORK
                || self.file_spec_work >= MAX_FILE_SPECULATIVE_WORK
            {
                self.spec_overflow = true;
                return TypeId::UNRESOLVED;
            }
            self.spec_work += 1;
            self.file_spec_work += 1;
        }
        self.depth += 1;
        let t = self.check_expr_inner(e);
        self.depth -= 1;
        if self.inst_overflow || self.tuple_overflow {
            self.report_instantiation_overflow(e, true);
        }
        // Reported at the innermost expression around the overflow that reports anything.
        if self.spec_overflow && self.speculative == 0 && self.reporting(e.file) {
            self.spec_overflow = false;
            if !std::mem::replace(&mut self.spec_reported, true) {
                self.error(
                    e,
                    dg::TYPE_INSTANTIATION_IS_EXCESSIVELY_DEEP_AND_POSSIBLY_INFINITE_2589,
                    &[],
                );
            }
        }
        if cacheable {
            self.node_types.insert(e, t);
        } else if spec_cacheable && self.speculative > 0 && self.flow_loop == 0 {
            self.spec_node_types.insert(e, (self.spec_epoch, t));
        } else if loop_cacheable && self.flow_loop > 0 {
            let level = self.flow_pending.len() as u32;
            self.loop_node_types.insert(e, (t, level));
        }
        if reporting {
            self.checked.insert(e);
        }
        t
    }

    /// Checks `e` with `ctx` as its contextual type (speculative checks of call arguments).
    pub fn check_expr_with_context(&mut self, e: Loc, ctx: TypeId) -> TypeId {
        self.context_overrides.push((e, ctx));
        self.push_assumptions();
        let t = self.check_expr(e);
        self.context_overrides.pop();
        self.pop_assumptions();
        t
    }

    fn check_expr_inner(&mut self, e: Loc) -> TypeId {
        let ast = self.ast(e.file);
        let f = e.file;
        let at = |n: NodeId| Loc::new(f, n);
        match *ast.node(e.node) {
            Node::NumberLit { value } => self.store().number_literal(value.get(), true),
            Node::StringLit { value } => self.store().string_literal(ast.atom(value), true),
            Node::BigIntLit { value } => {
                let text = bigint_text(ast.atom(value));
                self.store().bigint_literal(&text, true)
            }
            Node::BoolLit { value } => {
                if value {
                    TypeId::FRESH_TRUE
                } else {
                    TypeId::FRESH_FALSE
                }
            }
            Node::NullLit {} => {
                if self.c.opts.strict_null_checks {
                    TypeId::NULL
                } else {
                    TypeId::NULL_WIDENING
                }
            }
            Node::RegexLit { .. } => self
                .global_type(GlobalType::RegExp)
                .unwrap_or(TypeId::UNRESOLVED),
            Node::TemplateLit { parts } => {
                let ps = ast.list(parts);
                let mut texts = vec![String::new()];
                let mut types = Vec::new();
                for &p in ps {
                    match *ast.node(p) {
                        Node::TemplateElement { cooked, .. } => {
                            if let Some(last) = texts.last_mut() {
                                last.push_str(ast.atom(cooked));
                            }
                        }
                        _ => {
                            types.push(self.check_expr(at(p)));
                            texts.push(String::new());
                        }
                    }
                }
                if types.is_empty() {
                    self.store().string_literal(&texts[0], true)
                } else if self.in_const_context(e) || self.is_template_literal_context(e) {
                    // A template literal type when the context asks for one.
                    match self.template_literal_type(&texts, &types) {
                        TypeId::UNRESOLVED | TypeId::WILDCARD => TypeId::STRING,
                        t if self.store().is_error(t) => TypeId::STRING,
                        t => t,
                    }
                } else if let Some(v) = self.eval_constant(e, SymId::NONE, &[], 0)
                    && let Some(text) = self.store().string_value(v).map(str::to_owned)
                {
                    // A template of constants (`${E.a}`, `${k}` of `const k = "k"`) is that
                    // string literal (observed, tsc 7).
                    self.store().string_literal(&text, true)
                } else {
                    TypeId::STRING
                }
            }
            Node::TemplateElement { .. } => TypeId::STRING,
            Node::Ident { name } => {
                let text = ast.atom(name);
                self.check_identifier(e, text)
            }
            Node::This {} => self.check_this(e),
            Node::Super {} => self.check_super(e),
            Node::Paren { expr } => self.check_expr(at(expr)),
            Node::ArrayLit { .. } => self.check_array_literal(e),
            Node::ObjectLit { .. } => self.check_object_literal(e),
            Node::Member { .. } | Node::Index { .. } | Node::Call { .. } => {
                if self.in_optional_chain(e) {
                    let (t, short) = self.check_chain(e);
                    if short {
                        self.add_chain_undefined(t)
                    } else {
                        t
                    }
                } else {
                    match *ast.node(e.node) {
                        Node::Member { obj, prop, .. } => {
                            if let Some(t) = self.commonjs_module_exports_type(e) {
                                return t;
                            }
                            let ot = self.check_expr(at(obj));
                            self.property_access(e, obj, ot, prop, false)
                        }
                        Node::Index { obj, index, .. } => {
                            let ot = self.check_expr(at(obj));
                            self.element_access(e, obj, ot, index, false)
                        }
                        _ => self.check_call(e, None),
                    }
                }
            }
            Node::New { .. } => self.check_new(e),
            Node::TaggedTemplate {
                tag,
                quasi,
                type_args,
            } => {
                let tt = self.check_expr(at(tag));
                // An `any` tag (an unresolved import included) gives `any`, type arguments or not.
                // A `super` tag is a grammar error, not resolved.
                if tt == TypeId::UNRESOLVED
                    || self.store().flags(tt).intersects(TypeFlags::ANY)
                    || matches!(ast.node(tag), Node::Super {})
                {
                    self.check_expr(at(quasi));
                    return if tt == TypeId::UNRESOLVED {
                        TypeId::UNRESOLVED
                    } else {
                        TypeId::ANY
                    };
                }
                self.resolve_call_like(
                    e,
                    at(tag),
                    tt,
                    type_args,
                    crate::call::Args::Tagged(quasi),
                    crate::members::SigKind::Call,
                )
            }
            Node::FunctionExpr { .. } => self.check_function_expr(e),
            Node::Arrow { type_params, .. } => {
                if let Some(tp) = type_params {
                    self.check_reserved_arrow_type_params(e.file, tp);
                }
                self.check_function_expr(e)
            }
            Node::ClassExpr { .. } => {
                self.defer(e);
                match self.c.sym_of_decl(e) {
                    Some(s) => self.type_of_symbol(s),
                    None => TypeId::UNRESOLVED,
                }
            }
            Node::Unary { op, arg } => self.check_unary(e, op, arg),
            Node::Update { op, arg, .. } => self.check_update(e, op, arg),
            Node::Binary { left, op, right } => self.check_binary(e, left, op, right),
            Node::Assign { left, op, right } => self.check_assignment(e, left, op, right),
            Node::Conditional { test, cons, alt } => {
                self.check_truthiness_expr(at(test));
                let a = self.check_expr(at(cons));
                let b = self.check_expr(at(alt));
                self.union_with(&[a, b], Reduction::Subtype, AliasId::NONE)
            }
            Node::Seq { exprs } => {
                self.check_comma_left_sides(e);
                let mut t = TypeId::UNDEFINED;
                for &x in ast.list(exprs) {
                    t = self.check_expr(at(x));
                }
                t
            }
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                self.check_assertion(e, expr, ty)
            }
            Node::Satisfies { expr, ty } => {
                let target = self.type_from_node(at(ty));
                let t = self.check_expr(at(expr));
                // An unresolved target makes the whole expression the error type.
                if self.store().is_error(target) {
                    return target;
                }
                if !self.is_assignable(t, target)
                    && self.reporting(e.file)
                    && !self.report_jsdoc_satisfies(e, ty, t, target)
                {
                    self.report_assignability(
                        t,
                        target,
                        at(expr),
                        Some(at(expr)),
                        Some(Head {
                            info: dg::TYPE_0_DOES_NOT_SATISFY_THE_EXPECTED_TYPE_1_1360,
                        }),
                    );
                }
                t
            }
            Node::NonNull { expr } => {
                let t = self.check_expr(at(expr));
                let t = self.non_null_type_params(e, t);
                self.non_nullable(t)
            }
            Node::Await { arg } => {
                if self.in_sync_function(e) {
                    let start = self.c.span(e).start;
                    self.error_span(
                        e.file,
                        Span::new(start, start + "await".len() as u32),
                        dg::AWAIT_EXPRESSIONS_ARE_ONLY_ALLOWED_WITHIN_ASYNC_FUNCTIONS_AND_AT_THE_TOP_LEVELS_O_1308,
                        &[],
                    );
                } else if self.builds_into_node_commonjs(e.file)
                    && self.at_top_level(e)
                    && self.await_operand_starts_with_word(e)
                {
                    let start = self.c.span(e).start;
                    self.error_span(
                        e.file,
                        Span::new(start, start + "await".len() as u32),
                        dg::THE_CURRENT_FILE_IS_A_COMMONJS_MODULE_AND_CANNOT_USE_AWAIT_AT_THE_TOP_LEVEL_1309,
                        &[],
                    );
                }
                // TS2524 follows the TS2523 rule for `yield` (after any TS1308).
                if self.reporting(e.file) && self.in_parameter_initializer_of_container(e) {
                    self.error(
                        e,
                        dg::AWAIT_EXPRESSIONS_CANNOT_BE_USED_IN_A_PARAMETER_INITIALIZER_2524,
                        &[],
                    );
                }
                let t = self.check_expr(at(arg));
                self.awaited_type(t)
            }
            Node::Yield { arg, delegate } => {
                // Outside a generator only TS1163 is reported; the operand is not checked
                // (tsc 7: `yield foo` there has no TS2304 for `foo`).
                if !self.in_generator(e) {
                    self.error(
                        e,
                        dg::A_YIELD_EXPRESSION_IS_ONLY_ALLOWED_IN_A_GENERATOR_BODY_1163,
                        &[],
                    );
                    self.check_yield_expression(e, delegate);
                    return TypeId::UNRESOLVED;
                }
                self.check_yield_expression(e, delegate);
                let t = arg.map(|a| self.check_expr(at(a)));
                if delegate
                    && let (Some(a), Some(t)) = (arg, t)
                    && t != TypeId::NEVER
                    && let Some(func) = self.containing_function(e)
                    && let Some(parts) = crate::nodes::fn_parts(self.ast(e.file), func.node)
                {
                    let is_async = parts.flags.contains(ts_r_ast::FnFlags::ASYNC);
                    self.check_iterable(t, at(a), is_async);
                }
                // The operand (`undefined` for a bare `yield`, what a `yield*` operand
                // iterates) against the yield type of an annotated generator (awaited in an
                // async one).
                if let Some(func) = self.containing_function(e)
                    && let Some(parts) = crate::nodes::fn_parts(self.ast(e.file), func.node)
                    && parts.return_type.is_some()
                    && parts.flags.contains(ts_r_ast::FnFlags::GENERATOR)
                    && let Some((y, _)) = self.generator_context(func)
                    && y != TypeId::UNRESOLVED
                {
                    let is_async = parts.flags.contains(ts_r_ast::FnFlags::ASYNC);
                    match arg {
                        Some(a) if delegate => {
                            let t = t.unwrap_or(TypeId::UNRESOLVED);
                            if !self.store().flags(t).intersects(TypeFlags::ANY)
                                && let Some(it) = self.quietly(|ck| ck.iterated_type(t, is_async))
                            {
                                self.check_assignable_with_elaboration(it, y, at(a), None);
                            }
                        }
                        Some(a) => {
                            let t = t.unwrap_or(TypeId::UNRESOLVED);
                            let t = if is_async { self.awaited_type(t) } else { t };
                            self.check_assignable_with_elaboration(t, y, at(a), Some(at(a)));
                        }
                        None if delegate => {}
                        None => {
                            self.check_assignable_with_elaboration(
                                TypeId::UNDEFINED,
                                y,
                                e,
                                Some(e),
                            );
                        }
                    }
                }
                // The value a `yield` receives: `any` in a generator without a return
                // annotation or context (tsc: TS7057 under noImplicitAny, not reported here).
                if let Some(func) = self.containing_function(e)
                    && let Some(parts) = crate::nodes::fn_parts(self.ast(e.file), func.node)
                    && parts.return_type.is_none()
                    && self.contextual_return_type(func).is_none()
                {
                    return TypeId::ANY;
                }
                TypeId::UNRESOLVED
            }
            Node::Spread { arg } => self.check_expr(at(arg)),
            Node::ImportCall {
                arg,
                options,
                phase,
            } => {
                if phase == ts_r_ast::ImportPhase::Defer
                    && self.reporting(e.file)
                    && !self.deferred_imports_supported()
                {
                    self.error(
                        e,
                        dg::DEFERRED_IMPORTS_ARE_ONLY_SUPPORTED_WHEN_THE_MODULE_FLAG_IS_SET_TO_ESNEXT_OR_PRE_18060,
                        &[],
                    );
                }
                self.check_expr(at(arg));
                if let Some(o) = options {
                    self.check_expr(at(o));
                    use ts_r_config::ModuleKind as M;
                    if self.reporting(e.file)
                        && !self.c.file(e.file).kind.is_declaration()
                        && !matches!(
                            self.c.opts.module,
                            M::ESNext
                                | M::Node16
                                | M::Node18
                                | M::Node20
                                | M::NodeNext
                                | M::Preserve
                                | M::AMD
                                | M::UMD
                                | M::System
                        )
                    {
                        self.error(
                            at(o),
                            dg::DYNAMIC_IMPORTS_ONLY_SUPPORT_A_SECOND_ARGUMENT_WHEN_THE_MODULE_OPTION_IS_SET_TO_E_1324,
                            &[],
                        );
                    }
                    // `{ assert: ... }` (an identifier key) is deprecated: TS2880 at the key.
                    if let Node::ObjectLit { props } = *ast.node(o) {
                        for &p in ast.list(props) {
                            if let Node::Property { key, .. } = *ast.node(p)
                                && matches!(*ast.node(key), Node::Ident { name } if ast.atom(name) == "assert")
                            {
                                self.error(
                                    at(key),
                                    dg::IMPORT_ASSERTIONS_HAVE_BEEN_REPLACED_BY_IMPORT_ATTRIBUTES_USE_WITH_INSTEAD_OF_AS_2880,
                                    &[],
                                );
                            }
                        }
                    }
                }
                if matches!(ast.node(arg), Node::Missing {}) {
                    // `import()` is `Promise<any>`; `import.defer` without a call is not
                    // an import.
                    let span = self.c.span(e);
                    let called = self
                        .c
                        .file(e.file)
                        .text
                        .get(..span.end as usize)
                        .is_some_and(|t| t.ends_with(')'));
                    return if called {
                        self.promise_of(TypeId::ANY)
                    } else {
                        TypeId::UNRESOLVED
                    };
                }
                self.dynamic_import_type(e.file, arg)
                    .unwrap_or(TypeId::UNRESOLVED)
            }
            Node::Instantiation { expr, type_args } => {
                let t = self.check_expr(at(expr));
                let args = self.type_args_from_node(Some(at(type_args)), false);
                self.instantiation_expression_type(t, &args, Some(at(type_args)))
            }
            Node::MetaProperty { kind } => {
                if kind == ts_r_ast::MetaPropKind::ImportMeta
                    && self.builds_into_node_commonjs(e.file)
                {
                    self.error(
                        e,
                        dg::THE_IMPORT_META_META_PROPERTY_IS_NOT_ALLOWED_IN_FILES_WHICH_WILL_BUILD_INTO_COMM_1470,
                        &[],
                    );
                }
                let span = self.c.span(e);
                let written = self
                    .c
                    .file(e.file)
                    .text
                    .get(span.start as usize..span.end as usize)
                    .unwrap_or("");
                match kind {
                    // The global `ImportMeta` interface (lib.es5 declares it, lib.dom and
                    // @types/node add members); not for a misspelled `import.metal`.
                    ts_r_ast::MetaPropKind::ImportMeta if written == "import.meta" => self
                        .c
                        .global_symbol("ImportMeta", ts_r_binder::meaning::TYPE)
                        .map(|s| self.declared_type_of_symbol(s))
                        .unwrap_or(TypeId::UNRESOLVED),
                    _ => TypeId::UNRESOLVED,
                }
            }
            Node::Hole {} => TypeId::UNDEFINED,
            Node::Missing {} => TypeId::ERROR,
            // Without a `JSX` namespace (the factory's or the global one) there is no
            // `JSX.Element`: tsc gives the element its error type (observed,
            // `jsx_element_error_type`).
            Node::JsxElement { .. } | Node::JsxFragment { .. } => self.check_jsx_expr(e),
            Node::JsxMemberExpr { obj, prop } => {
                let ot = self.check_expr(at(obj));
                self.property_access(e, obj, ot, prop, false)
            }
            _ => TypeId::UNRESOLVED,
        }
    }

    /// `import("./m")` of a TypeScript ES module of the program: `Promise<typeof import("./m")>`
    /// (`dynamic_import_type`); `Promise<any>` for a specifier that is not a string literal
    /// or does not resolve (tsc reports the latter, TS2307). Other targets (JavaScript,
    /// `export =`, ambient) are not modelled.
    fn dynamic_import_type(
        &mut self,
        file: ts_r_diagnostics::FileId,
        arg: NodeId,
    ) -> Option<TypeId> {
        match self.ast(file).node(arg) {
            Node::StringLit { .. } => {}
            _ => return Some(self.promise_of(TypeId::ANY)),
        }
        let req = self.c.request_of(file, arg)?;
        let target = match self.c.link.module_target(file, req) {
            ts_r_binder::program::ModuleTarget::File(t) => t,
            ts_r_binder::program::ModuleTarget::Unresolved => {
                return Some(self.promise_of(TypeId::ANY));
            }
            ts_r_binder::program::ModuleTarget::Ambient(_) => return None,
        };
        let tf = self.c.file(target);
        if !tf.bound.is_module || tf.is_js() {
            return None;
        }
        // Under the Node module kinds a CommonJS-format target is wrapped (its exports
        // become a synthetic `default`); not modelled.
        use ts_r_config::ModuleKind as M;
        if matches!(
            self.c.opts.module,
            M::Node16 | M::Node18 | M::Node20 | M::NodeNext
        ) && !tf.esm_format
        {
            return None;
        }
        let msym = self.c.sym(target, tf.bound.file_symbol);
        if self.c.export_of(msym, "export=").is_some() {
            return None;
        }
        let ns = self.type_of_symbol(msym);
        if ns == TypeId::UNRESOLVED {
            return None;
        }
        Some(self.promise_of(ns))
    }

    /// Defers checking a function or class body to the per-file walk.
    pub fn defer(&mut self, e: Loc) {
        if self.reporting(e.file) && self.deferred_set.insert(e) {
            self.deferred.push(e);
        }
    }

    /// Whether `file` is a non-declaration file that `node16`..`nodenext` emit as CommonJS
    /// (its format, from its extension or the nearest `package.json`, is not ESM).
    pub fn builds_into_node_commonjs(&self, file: ts_r_diagnostics::FileId) -> bool {
        use ts_r_config::ModuleKind as M;
        let f = self.c.file(file);
        matches!(
            self.c.opts.module,
            M::Node16 | M::Node18 | M::Node20 | M::NodeNext
        ) && !f.package_esm
            && !f.kind.is_declaration()
            && f.bound.is_module
    }

    /// Whether the operand of the `await` expression `e` starts with an identifier, keyword
    /// or literal. tsc 7 reports TS1309 only then: an operand starting with punctuation
    /// (`await (x)`, `await [x]`, `await <T>x`) is not reported.
    fn await_operand_starts_with_word(&self, e: Loc) -> bool {
        let span = self.c.span(e);
        let text = &self.c.file(e.file).text;
        let rest = text
            .get(span.start as usize + "await".len()..span.end as usize)
            .unwrap_or("");
        rest.trim_start().chars().next().is_some_and(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '$' | '\'' | '"' | '\\') || !ch.is_ascii()
        })
    }

    /// TS7060: in a `.mts`/`.cts` file, `<T>() => ...` (one unconstrained type parameter, no
    /// trailing comma) reads as a type assertion in JavaScript-flavoured parsers.
    fn check_reserved_arrow_type_params(&mut self, file: ts_r_diagnostics::FileId, tp: NodeId) {
        let f = self.c.file(file);
        let lower = f.path.to_ascii_lowercase();
        if !(lower.ends_with(".mts") || lower.ends_with(".cts")) || !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let Node::TypeParams { params } = *ast.node(tp) else {
            return;
        };
        let [only] = ast.list(params) else {
            return;
        };
        let Node::TypeParam {
            constraint: None, ..
        } = *ast.node(*only)
        else {
            return;
        };
        let mut end = ast.span(*only).end as usize;
        let comments =
            &ast.comments[ast.comments.partition_point(|c| c.span.end as usize <= end)..];
        for c in comments {
            if c.span.start >= ast.span(tp).end
                || !f
                    .text
                    .get(end..c.span.start as usize)
                    .is_some_and(|rest| rest.trim().is_empty())
            {
                break;
            }
            end = c.span.end as usize;
        }
        let trailing_comma = f
            .text
            .get(end..ast.span(tp).end as usize)
            .is_some_and(|rest| rest.trim_start().starts_with(','));
        if !trailing_comma {
            self.error(
                Loc::new(file, *only),
                dg::THIS_SYNTAX_IS_RESERVED_IN_FILES_WITH_THE_MTS_OR_CTS_EXTENSION_ADD_A_TRAILING_CO_7060,
                &[],
            );
        }
    }

    /// Whether `e` is outside every function and class body.
    pub fn at_top_level(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::PropertyDef { .. }
                | Node::StaticBlock { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        true
    }

    /// Whether `e` is directly inside a function, function expression or arrow that is not
    /// async (class bodies and the top level are not).
    fn in_sync_function(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::FunctionDecl { flags, .. }
                | Node::FunctionExpr { flags, .. }
                | Node::Arrow { flags, .. } => {
                    return !flags.contains(ts_r_ast::FnFlags::ASYNC);
                }
                Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::PropertyDef { .. }
                | Node::StaticBlock { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// TS2464 at computed name `key` whose expression has type `kt` when that is not a
    /// valid key: valid keys are assignable to `string | number | symbol` (`never`,
    /// `K extends keyof U`, `S extends string` too).
    pub(crate) fn check_computed_key_type(&mut self, key: Loc, kt: TypeId) -> bool {
        let keys = self.union(&[TypeId::STRING, TypeId::NUMBER, TypeId::SYMBOL]);
        if !self.store().flags(kt).intersects(
            TypeFlags::STRING_LIKE
                | TypeFlags::NUMBER_LIKE
                | TypeFlags::ES_SYMBOL_LIKE
                | TypeFlags::ANY
                | TypeFlags::NEVER,
        ) && !self.every_member(kt, |m| {
            self.store().flags(m).intersects(
                TypeFlags::STRING_LIKE | TypeFlags::NUMBER_LIKE | TypeFlags::ES_SYMBOL_LIKE,
            )
        }) && (self.nullable_facts(kt) != (false, false)
            || !self.quietly(|ck| ck.is_assignable(kt, keys)))
        {
            self.error(
                key,
                dg::A_COMPUTED_PROPERTY_NAME_MUST_BE_OF_TYPE_STRING_NUMBER_SYMBOL_OR_ANY_2464,
                &[],
            );
            return true;
        }
        false
    }

    fn in_generator(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        // Decorators, `extends` expressions and computed member names run in the scope
        // around their class: inside a generator they may `yield`.
        let is_class =
            |n: NodeId| matches!(ast.node(n), Node::ClassDecl { .. } | Node::ClassExpr { .. });
        let mut outside_class = false;
        let mut cur = parents.parent(e.node);
        // The two nodes below `cur` on the path from `e`.
        let (mut prev, mut prev2) = (e.node, e.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::Decorator { .. } | Node::HeritageClause { .. } => outside_class = true,
                Node::ComputedKey { .. }
                    if parents
                        .parent(n)
                        .and_then(|m| parents.parent(m))
                        .is_some_and(is_class) =>
                {
                    outside_class = true;
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } if outside_class => {
                    outside_class = false;
                }
                Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. } => {
                    return flags.contains(ts_r_ast::FnFlags::GENERATOR);
                }
                Node::Arrow { .. } => return false,
                // A class's heritage clauses and computed member names belong to the
                // enclosing function; its member bodies and initializers don't.
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    let outer = match *ast.node(prev) {
                        Node::HeritageClause { .. } => true,
                        Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => {
                            key == prev2 && matches!(ast.node(key), Node::ComputedKey { .. })
                        }
                        _ => false,
                    };
                    if !outer {
                        return false;
                    }
                }
                _ => {}
            }
            prev2 = prev;
            prev = n;
            cur = parents.parent(n);
        }
        false
    }

    // ---------------------------------------------------------------------------------
    // Identifiers, `this`, `super`
    // ---------------------------------------------------------------------------------

    fn check_identifier(&mut self, e: Loc, name: &str) -> TypeId {
        let Some(s) = self.c.resolve_ident(e, meaning::VALUE) else {
            return self.unresolved_identifier(e, name);
        };
        if self.check_body_local_from_parameter(e, s, name) {
            return TypeId::ERROR;
        }
        // `exports` declared ambiently (`declare var exports: any`) in a CommonJS module
        // with `module.exports = v` is still the exports object.
        if name == "exports"
            && self.c.file(e.file).is_js()
            && self.is_commonjs_js_file(e.file)
            && self.is_commonjs_global(e)
            && let Some(t) = self.commonjs_export_equals_type(e)
        {
            return t;
        }
        let mut sym = s;
        if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
            match self.c.resolve_alias(s) {
                // A type-only import merged with a local value (`import type I = ...;
                // const I = {}`): the value is the local declaration.
                Some(t)
                    if t != s
                        && !self.c.has_meaning(t, meaning::VALUE)
                        && self.c.sym_flags(s).intersects(
                            SymbolFlags::VARIABLE
                                | SymbolFlags::FUNCTION
                                | SymbolFlags::CLASS
                                | SymbolFlags::REGULAR_ENUM
                                | SymbolFlags::VALUE_MODULE,
                        ) => {}
                Some(t)
                    if t != s
                        && !self.c.has_meaning(t, meaning::VALUE)
                        && self.c.value_merged_in_alias_chain(s).is_some() =>
                {
                    sym = self.c.value_merged_in_alias_chain(s).unwrap_or(s);
                }
                Some(t) if t != s => {
                    if let Some(v) = self.json_import_type(s, t) {
                        self.check_type_only_alias_use(e, s, t, name);
                        return v;
                    }
                    // An alias further along merged with a local value (`import { A }` and
                    // `const A = 0` re-exported): that value.
                    let t = if !self.c.has_meaning(t, meaning::VALUE)
                        && let Some(v) = self.alias_merged_value(s)
                    {
                        v
                    } else {
                        t
                    };
                    if !self.c.has_meaning(t, meaning::VALUE) {
                        if let Some(v) = self.export_equals_member_type(s) {
                            return v;
                        }
                        if self.c.has_meaning(t, meaning::TYPE) {
                            self.error(
                                e,
                                dg::_0_ONLY_REFERS_TO_A_TYPE_BUT_IS_BEING_USED_AS_A_VALUE_HERE_2693,
                                &[name],
                            );
                        } else {
                            self.error(e, dg::CANNOT_USE_NAMESPACE_0_AS_A_VALUE_2708, &[name]);
                        }
                        return TypeId::ERROR;
                    }
                    self.check_type_only_alias_use(e, s, t, name);
                    sym = t;
                }
                None => match self.c.synthetic_default_module(s) {
                    // A JSON module's synthetic default is its value.
                    Some(m) => match self.json_value_type(m) {
                        Some(v) => return v,
                        None => sym = m,
                    },
                    // A named import of a property of an `export =` value
                    // (`import { resolve } from "path"`).
                    None => {
                        return self.export_equals_member_type(s).unwrap_or(TypeId::ERROR);
                    }
                },
                _ => return TypeId::ERROR,
            }
        }
        self.check_use_before_declaration(e, sym, name);
        self.check_parameter_reference(e, sym, name);
        self.check_constructor_shadowed_reference(e, Some(sym), name);
        if self.c.sym_flags(sym).intersects(SymbolFlags::CONST_ENUM) {
            self.check_const_enum_reference(e);
        }
        let declared = self.type_of_symbol(sym);
        let f = self.c.sym_flags(sym);
        if f.intersects(SymbolFlags::VARIABLE) && !f.intersects(SymbolFlags::ALIAS) {
            return self.narrowed_identifier_type(e, sym, declared, name);
        }
        // A namespace import of an `export =` value (`import * as b from "./b"` with
        // `export = b` of a `number | undefined`) narrows like a constant.
        if sym != s
            && self.store().flags(declared).contains(TypeFlags::UNION)
            && self.is_narrowable_reference(e)
        {
            return self.flow_type_of_reference(e, declared, declared);
        }
        declared
    }

    /// The first alias after `s` on its chain that is merged with a local value
    /// (`import { A } from "./a"; const A = 0; export { A }`).
    pub(crate) fn alias_merged_value(&self, s: SymId) -> Option<SymId> {
        let r = self.c.parts(s).next()?;
        for a in self.c.link.alias_chain(r).into_iter().skip(1) {
            let f = self.c.file(a.file);
            if f.bound.symbol(a.symbol).flags.intersects(
                SymbolFlags::VARIABLE
                    | SymbolFlags::FUNCTION
                    | SymbolFlags::CLASS
                    | SymbolFlags::REGULAR_ENUM
                    | SymbolFlags::VALUE_MODULE,
            ) {
                return Some(self.c.sym_ref(a));
            }
        }
        None
    }

    /// TS1361/TS1362: a value read through an alias chain with a type-only link (`import
    /// type`, `export type`, or a `type` specifier); the nearest such link decides the message.
    /// The value's type is still used.
    fn check_type_only_alias_use(&mut self, e: Loc, s: SymId, target: SymId, name: &str) {
        if !self.reporting(e.file)
            || self.c.file(e.file).kind.is_declaration()
            || self.type_only_use_allowed(e)
        {
            return;
        }
        let Some(r) = self.c.parts(s).next() else {
            return;
        };
        // A target merged across files (a module augmentation of a re-exported name) has
        // value declarations outside the type-only chain.
        let mut files = self.c.parts(target).map(|p| p.file);
        if let Some(first) = files.next()
            && files.any(|f| f != first)
        {
            return;
        }
        for a in self.c.link.alias_chain(r) {
            let f = self.c.file(a.file);
            let sym = f.bound.symbol(a.symbol);
            // An alias merged with a local value (`import { A }` and `namespace A`) exports
            // that value.
            if sym.flags.intersects(
                SymbolFlags::VARIABLE
                    | SymbolFlags::FUNCTION
                    | SymbolFlags::CLASS
                    | SymbolFlags::REGULAR_ENUM
                    | SymbolFlags::VALUE_MODULE,
            ) {
                return;
            }
            if !sym.facts.intersects(SymbolFacts::TYPE_ONLY) {
                continue;
            }
            let exported = f.bound.decls(a.symbol).first().is_some_and(|&d| {
                !matches!(
                    f.ast.node(d),
                    Node::ImportSpecifier { .. }
                        | Node::ImportDefaultSpecifier { .. }
                        | Node::ImportNamespaceSpecifier { .. }
                        | Node::ImportEquals { .. }
                        | Node::ImportDecl { .. }
                )
            });
            let info = if exported {
                dg::_0_CANNOT_BE_USED_AS_A_VALUE_BECAUSE_IT_WAS_EXPORTED_USING_EXPORT_TYPE_1362
            } else {
                dg::_0_CANNOT_BE_USED_AS_A_VALUE_BECAUSE_IT_WAS_IMPORTED_USING_IMPORT_TYPE_1361
            };
            self.error(e, info, &[name]);
            return;
        }
    }

    /// Uses of a type-only alias that emit nothing: `export =` / `export default` of it, type
    /// positions (`typeof`, interfaces, type literals), ambient (`declare`) declarations and
    /// the computed names of abstract members.
    fn type_only_use_allowed(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut child = e.node;
        let mut cur = parents.parent(e.node);
        let mut first = true;
        while let Some(n) = cur {
            let node = ast.node(n);
            match *node {
                Node::ExportAssignment { .. } | Node::ExportDefaultExpr { .. } if first => {
                    return true;
                }
                Node::Paren { .. } => {}
                Node::Interface { .. }
                | Node::TypeAlias { .. }
                | Node::TypeLiteral { .. }
                | Node::TypeQuery { .. } => return true,
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. }
                    if key == child && node.modifiers().contains(ts_r_ast::Modifiers::ABSTRACT) =>
                {
                    return true;
                }
                _ => first = false,
            }
            if node.modifiers().contains(ts_r_ast::Modifiers::DECLARE) {
                return true;
            }
            child = n;
            cur = parents.parent(n);
        }
        false
    }

    fn unresolved_identifier(&mut self, e: Loc, name: &str) -> TypeId {
        match name {
            "undefined" => {
                return if self.c.opts.strict_null_checks {
                    TypeId::UNDEFINED
                } else {
                    TypeId::UNDEFINED_WIDENING
                };
            }
            "arguments" if self.in_non_arrow_function(e) => {
                return self
                    .global_type(GlobalType::IArguments)
                    .unwrap_or(TypeId::UNRESOLVED);
            }
            "globalThis" if self.c.resolve_ident(e, meaning::VALUE).is_none() => {
                return self.global_this_type();
            }
            // JavaScript CommonJS: `require("x")` calls, and `module`/`exports` in a CommonJS
            // module, are not unresolved names (their types are not modelled).
            "require" if self.c.file(e.file).is_js() && self.is_require_call_callee(e) => {
                return TypeId::ANY;
            }
            "exports" if self.c.file(e.file).is_js() && self.is_commonjs_js_file(e.file) => {
                return self.commonjs_export_equals_type(e).unwrap_or(TypeId::ANY);
            }
            "module" if self.c.file(e.file).is_js() && self.is_commonjs_js_file(e.file) => {
                return TypeId::ANY;
            }
            _ => {}
        }
        if let Some(t) = self.c.resolve_ident(e, meaning::TYPE | meaning::NAMESPACE) {
            let t = self.c.resolve_alias(t).unwrap_or(t);
            if self.c.has_meaning(t, meaning::TYPE) {
                self.error(
                    e,
                    dg::_0_ONLY_REFERS_TO_A_TYPE_BUT_IS_BEING_USED_AS_A_VALUE_HERE_2693,
                    &[name],
                );
            } else {
                self.error(e, dg::CANNOT_USE_NAMESPACE_0_AS_A_VALUE_2708, &[name]);
            }
            return TypeId::ERROR;
        }
        // A constructor local read by an instance property initializer (TS2301).
        if self.check_constructor_shadowed_reference(e, None, name) {
            return TypeId::ERROR;
        }
        self.report_cannot_find_name(e, name);
        TypeId::ERROR
    }

    /// Whether the member `m` of an object or array literal is inside an assignment
    /// target (`({ a = 1 } = o)`, `for ({ a } of xs)`).
    fn is_destructuring_target(&self, m: Loc) -> bool {
        let ast = self.ast(m.file);
        let parents = self.c.file(m.file).parents();
        let mut cur = m.node;
        for _ in 0..256 {
            let Some(p) = parents.parent(cur) else {
                return false;
            };
            match *ast.node(p) {
                Node::Paren { .. }
                | Node::ObjectLit { .. }
                | Node::ArrayLit { .. }
                | Node::Property { .. }
                | Node::ShorthandProp { .. }
                | Node::Spread { .. } => cur = p,
                Node::Assign { left, .. } => return left == cur,
                Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == cur,
                _ => return false,
            }
        }
        false
    }

    /// Whether `e` starts a line or follows `{`, `,` or `;` (a comment counts as a start).
    fn starts_object_member(&self, e: Loc) -> bool {
        let text = self.c.file(e.file).text.as_bytes();
        let mut i = self.ast(e.file).span(e.node).start as usize;
        while i > 0 {
            match text.get(i - 1) {
                Some(b' ' | b'\t') => i -= 1,
                Some(b'\n' | b'\r' | b'{' | b',' | b';' | b'/') => return true,
                _ => return false,
            }
        }
        true
    }

    /// Whether identifier `e` is the callee of `require("x")` (one string literal argument).
    /// TS2475: a `const` enum object is only read for a member (`E.A`, `E["A"]`), or named
    /// by an export assignment.
    fn check_const_enum_reference(&mut self, e: Loc) {
        if !self.reporting(e.file) {
            return;
        }
        let ast = self.ast(e.file);
        let allowed = match self.c.file(e.file).parents().parent(e.node) {
            Some(p) => match *ast.node(p) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => obj == e.node,
                Node::ExportAssignment { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ImportEquals { .. } => true,
                _ => false,
            },
            None => false,
        };
        if !allowed {
            self.error(
                e,
                dg::CONST_ENUMS_CAN_ONLY_BE_USED_IN_PROPERTY_OR_INDEX_ACCESS_EXPRESSIONS_OR_THE_RIGH_2475,
                &[],
            );
        }
    }

    /// Whether `obj` names a `const` enum object (`E` in `E[k]`).
    fn is_const_enum_object(&mut self, obj: Loc) -> bool {
        let ast = self.ast(obj.file);
        let n = skip_parens(ast, obj.node);
        if !matches!(ast.node(n), Node::Ident { .. }) {
            return false;
        }
        let Some(s) = self.c.resolve_ident(Loc::new(obj.file, n), meaning::VALUE) else {
            return false;
        };
        let s = self.c.resolve_alias(s).unwrap_or(s);
        self.c.sym_flags(s).intersects(SymbolFlags::CONST_ENUM)
    }

    fn is_require_call_callee(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let Some(p) = self.c.file(e.file).parents().parent(e.node) else {
            return false;
        };
        match *ast.node(p) {
            Node::Call { callee, args, .. } => {
                callee == e.node
                    && ast.list(args).len() == 1
                    && matches!(ast.node(ast.list(args)[0]), Node::StringLit { .. })
            }
            _ => false,
        }
    }

    /// Whether a JavaScript file is a CommonJS module: it is no ES module (no top-level
    /// `import`/`export`) and somewhere assigns `exports.x`/`module.exports` or calls
    /// `require("x")`.
    pub(crate) fn is_commonjs_js_file(&mut self, file: ts_r_diagnostics::FileId) -> bool {
        self.c.file(file).is_commonjs_module()
    }

    /// The type of the `exports` object (`exports`, `module.exports`) at `e` in a JavaScript
    /// CommonJS module with `module.exports = v`: the type of `v` (`exports.x = ...` then
    /// reads and writes its properties). `any` inside `v` itself; `None` without one.
    pub(crate) fn commonjs_export_equals_type(&mut self, e: Loc) -> Option<TypeId> {
        let b = self.c.bound(e.file);
        if !b.commonjs {
            return None;
        }
        let m = self.c.sym(e.file, b.file_symbol);
        let Some(eq) = self.c.export_of(m, "export=") else {
            // No `module.exports = v`: the module's exports object.
            let t = self.type_of_symbol(m);
            if t == TypeId::UNRESOLVED || self.store().is_error(t) {
                return Some(TypeId::ANY);
            }
            return Some(t);
        };
        let decls = self.c.decls(eq);
        // Several `module.exports = v`: their union is not modelled (see `type_of_symbol`).
        if decls.len() > 1 {
            return Some(TypeId::ANY);
        }
        let ast = self.ast(e.file);
        let at = ast.span(e.node);
        for d in decls {
            if d.file == e.file
                && let Node::Assign { right, .. } = *ast.node(d.node)
            {
                let r = ast.span(right);
                if r.start <= at.start && at.end <= r.end {
                    return Some(TypeId::ANY);
                }
            }
        }
        let t = self.type_of_symbol(eq);
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return Some(TypeId::ANY);
        }
        Some(t)
    }

    /// Whether `l` (`exports.x`, `module.exports.x`) declares a CommonJS export of a module
    /// without `module.exports = v`: the assignment declares the export's type, it is not
    /// checked against it.
    fn is_commonjs_export_declaration(&self, l: Loc) -> bool {
        let b = self.c.bound(l.file);
        if !b.commonjs || !self.c.file(l.file).is_js() {
            return false;
        }
        let m = self.c.sym(l.file, b.file_symbol);
        self.c.export_of(m, "export=").is_none()
            && self.c.sym_of_decl(l).is_some_and(|s| {
                self.c
                    .sym_facts(s)
                    .intersects(ts_r_binder::SymbolFacts::EXPANDO)
                    && self.c.sym_parent(s) == Some(m)
            })
    }

    /// Whether identifier `e` (`require`, `module`, `exports`) is CommonJS's own: undeclared,
    /// or an ambient function or variable (`declare var module: { exports: any }`).
    pub(crate) fn is_commonjs_global(&self, e: Loc) -> bool {
        let Some(s) = self.c.resolve_ident(e, meaning::VALUE) else {
            return true;
        };
        let f = self.c.sym_flags(s);
        f.intersects(SymbolFlags::FUNCTION | SymbolFlags::VARIABLE)
            && !f.intersects(SymbolFlags::ALIAS)
            && self
                .c
                .decls(s)
                .first()
                .is_some_and(|&d| d.file != e.file && self.is_ambient_node(d))
    }

    /// `module.exports` (`module` undeclared or ambient) in a JavaScript CommonJS module
    /// with `module.exports = v`: the type of `v`.
    fn commonjs_module_exports_type(&mut self, e: Loc) -> Option<TypeId> {
        if !self.c.file(e.file).is_js() {
            return None;
        }
        let ast = self.ast(e.file);
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = *ast.node(e.node)
        else {
            return None;
        };
        if crate::nodes::ident_text(ast, obj) != Some("module")
            || crate::nodes::ident_text(ast, prop) != Some("exports")
            || !self.is_commonjs_js_file(e.file)
            || !self.is_commonjs_global(Loc::new(e.file, obj))
        {
            return None;
        }
        self.commonjs_export_equals_type(e)
    }

    /// TS2304 and its "did you mean" / missing-types variants.
    pub fn report_cannot_find_name(&mut self, e: Loc, name: &str) {
        if !self.reporting(e.file) {
            return;
        }
        if let Some(info) = self.primitive_heritage_error(e, name) {
            self.error(e, info, &[name]);
            return;
        }
        // A primitive type's name used as a value (`f(number)`; observed tsc 7).
        if matches!(
            name,
            "any" | "string" | "number" | "boolean" | "never" | "unknown"
        ) {
            self.error(
                e,
                dg::_0_ONLY_REFERS_TO_A_TYPE_BUT_IS_BEING_USED_AS_A_VALUE_HERE_2693,
                &[name],
            );
            return;
        }
        if let Some((info, lib)) = missing_types_hint(name) {
            self.error(e, info, &[name, lib]);
            return;
        }
        if self.in_with_body(e) {
            // tsc: every name in a `with` block is `any`.
            return;
        }
        if let Some((info, class)) = self.class_member_suggestion(e, name) {
            self.error(e, info, &[name, &class]);
            return;
        }
        // An export specifier's name may be of any meaning (`export type { T }`).
        let ast = self.ast(e.file);
        let m = match self.c.parent(e).map(|p| ast.node(p.node)) {
            Some(Node::ExportSpecifier { .. }) => meaning::ALL,
            _ => meaning::VALUE,
        };
        let names = self.scope_name_candidates(e, m, false);
        if let Some(s) = crate::relate::spelling_suggestion(name, names.iter().map(String::as_str))
        {
            self.error(e, dg::CANNOT_FIND_NAME_0_DID_YOU_MEAN_1_2552, &[name, &s]);
            return;
        }
        // `{ a }` (or `{ a = 1 } = o`) without a value `a`; not a name the parser recovered
        // as a shorthand after another token on its line (`{ class C {} }`, `{ a.b }`). A
        // `{ a = 1 }` outside a destructuring target reports only its `=` (TS1312).
        let ast = self.ast(e.file);
        let shorthand = self.c.parent(e).and_then(|p| match *ast.node(p.node) {
            Node::ShorthandProp { name: n, init, .. } if n == e.node => Some((p, init)),
            _ => None,
        });
        if let Some((p, Some(_))) = shorthand
            && !self.is_destructuring_target(p)
        {
            return;
        }
        if shorthand.is_some() && self.starts_object_member(e) {
            self.error(
                e,
                dg::NO_VALUE_EXISTS_IN_SCOPE_FOR_THE_SHORTHAND_PROPERTY_0_EITHER_DECLARE_ONE_OR_PROV_18004,
                &[name],
            );
            return;
        }
        self.error(e, dg::CANNOT_FIND_NAME_0_2304, &[name]);
    }

    /// Whether `e` is inside the body of a `with` statement.
    fn in_with_body(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut at = e.node;
        let mut cur = parents.parent(at);
        while let Some(n) = cur {
            if let Node::With { body, .. } = *ast.node(n)
                && body == at
            {
                return true;
            }
            at = n;
            cur = parents.parent(n);
        }
        false
    }

    /// TS2662 / TS2663 for an unresolved name inside a class member that names a member of
    /// that class (inherited ones too): an instance member declared in a class outside
    /// static members (`Did you mean the instance member 'this.x'?`), else a property of the
    /// static side, `Function` and `Object` members included, anywhere in the class
    /// (`'C.x'`). Arrow functions are transparent; another function, or no
    /// member at all (heritage clauses, computed keys), gets no suggestion. Observed in
    /// tsc 7.
    fn class_member_suggestion(
        &mut self,
        e: Loc,
        name: &str,
    ) -> Option<(dg::DiagnosticInfo, String)> {
        // `arguments` there is TS2815 in tsc (not modelled).
        if name == "arguments" {
            return None;
        }
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut at = e.node;
        let mut cur = parents.parent(at);
        let mut member: Option<(NodeId, bool)> = None;
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::FunctionExpr { .. } | Node::FunctionDecl { .. } => {
                    let p = parents.parent(n)?;
                    let Node::MethodDef { modifiers, .. } = *ast.node(p) else {
                        return None;
                    };
                    member = Some((p, modifiers.contains(ts_r_ast::Modifiers::STATIC)));
                    break;
                }
                Node::PropertyDef {
                    modifiers, value, ..
                } if value == Some(at) => {
                    member = Some((n, modifiers.contains(ts_r_ast::Modifiers::STATIC)));
                    break;
                }
                Node::StaticBlock { .. } => {
                    member = Some((n, true));
                    break;
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => return None,
                _ => {}
            }
            at = n;
            cur = parents.parent(n);
        }
        let (m, is_static) = member?;
        let class = parents.parent(m)?;
        if !matches!(
            ast.node(class),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        ) {
            return None;
        }
        let cs = self.c.sym_of_decl(Loc::new(e.file, class))?;
        let class_name = self.c.sym_name(cs).to_owned();
        // Instance members declared in a class body (not the `Object` members).
        let class_member = |ck: &mut Self, p: Option<ts_r_types::SymId>| {
            let Some(p) = p else { return false };
            let Some(d) = ck.c.value_decl(p).or_else(|| ck.c.first_decl(p)) else {
                return false;
            };
            match ck.node(d) {
                Node::PropertyDef { .. } | Node::MethodDef { .. } => {
                    ck.c.parent(d).is_some_and(|c| {
                        matches!(ck.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                    })
                }
                Node::Param { .. } => true,
                _ => false,
            }
        };
        if !is_static {
            let it = self.declared_type_of_symbol(cs);
            let p = self.property_of_type(it, name);
            if class_member(self, p) {
                return Some((
                    dg::CANNOT_FIND_NAME_0_DID_YOU_MEAN_THE_INSTANCE_MEMBER_THIS_0_2663,
                    String::new(),
                ));
            }
        }
        // An anonymous class expression is named by the variable it initializes.
        let class_name = match *ast.node(class) {
            Node::ClassExpr { name: None, .. } => {
                let p = parents.parent(class).map(|p| (p, ast.node(p)));
                match p {
                    Some((
                        _,
                        &Node::VarDeclarator {
                            name,
                            init: Some(i),
                            ..
                        },
                    )) if i == class && matches!(ast.node(name), Node::Ident { .. }) => {
                        crate::nodes::ident_text(ast, name).map(str::to_owned)
                    }
                    _ => None,
                }
            }
            _ => Some(class_name),
        };
        if let Some(class_name) = class_name {
            // The static side's apparent members count too (`D.prototype`, `D.bind`).
            let st = self.type_of_symbol(cs);
            if self.property_of_type(st, name).is_some() {
                return Some((
                    dg::CANNOT_FIND_NAME_0_DID_YOU_MEAN_THE_STATIC_MEMBER_1_0_2662,
                    class_name,
                ));
            }
        }
        None
    }

    /// A spelling suggestion among the value names visible at `e` (TS2552 for value
    /// references).
    pub fn name_suggestion(&mut self, e: Loc, name: &str) -> Option<String> {
        let names = self.scope_name_candidates(e, meaning::VALUE, false);
        crate::relate::spelling_suggestion(name, names.iter().map(|s| s.as_str()))
    }

    /// A spelling suggestion among the type names visible at `e` (TS2552 for type
    /// references).
    pub fn type_name_suggestion(&mut self, e: Loc, name: &str) -> Option<String> {
        let names = self.scope_name_candidates(e, meaning::TYPE, false);
        crate::relate::spelling_suggestion(name, names.iter().map(|s| s.as_str()))
    }

    /// The names of meaning `m` in scope at `e`, in the order tsc's suggestions visit them
    /// (observed with tsc 7): for types the primitive type keywords, for values
    /// `undefined`; then each enclosing scope's locals and, for a namespace or module file,
    /// its exports; then the globals in declaration order (lib files first, so a lib's
    /// `Lock` is preferred to a script's `ELoc` at the same distance). A script's own
    /// top-level declarations are globals and come with them. Symbols without the meaning
    /// are not candidates (a type-only `A` is no suggestion for the value `a`), and neither
    /// are, with `skip_declaring`, symbols declared around `e` (the alias being declared,
    /// `import v = V`).
    pub(crate) fn scope_name_candidates(
        &mut self,
        e: Loc,
        m: u8,
        skip_declaring: bool,
    ) -> Vec<String> {
        let at = self.c.span(e);
        let declaring = |ck: &Self, s: ts_r_types::SymId| {
            skip_declaring
                && ck.c.decls(s).iter().any(|d| {
                    let ds = ck.c.span(*d);
                    d.file == e.file && ds.start <= at.start && at.end <= ds.end
                })
        };
        let b = self.c.bound(e.file);
        let ast = self.ast(e.file);
        let mut names: Vec<String> = Vec::new();
        if m == meaning::TYPE {
            names.extend(
                ["string", "number", "boolean", "bigint", "symbol", "object"]
                    .iter()
                    .map(|s| (*s).to_owned()),
            );
        }
        if m & meaning::VALUE != 0 {
            names.push("undefined".to_owned());
        }
        let parents = self.c.file(e.file).parents();
        let mut cur = Some(e.node);
        let mut scope = None;
        while let Some(n) = cur {
            if let Some(sc) = b.scope_of(n) {
                scope = Some(sc);
                break;
            }
            cur = parents.parent(n);
        }
        let usable = |ck: &Self, s: ts_r_types::SymId| {
            let f = ck.c.sym_flags(s);
            ck.c.has_meaning(s, m) && !declaring(ck, s) && {
                // Class members and enum members merged into a namespace are not in
                // scope in its body.
                let own = f.without(
                    SymbolFlags::PROPERTY
                        | SymbolFlags::METHOD
                        | SymbolFlags::ACCESSOR
                        | SymbolFlags::ENUM_MEMBER,
                );
                crate::symbols::symbol_meaning(own) & m != 0
            }
        };
        for _ in 0..1000 {
            let Some(sc) = scope else { break };
            let Some(data) = b.scopes.get(sc.0 as usize) else {
                break;
            };
            let script_file = data.kind == ts_r_binder::ScopeKind::File && !b.is_module;
            if !script_file {
                for (n, sym) in b.table(data.locals) {
                    let s = self.c.sym(e.file, *sym);
                    if usable(self, s) {
                        names.push(b.names.text(ast, *n).to_owned());
                    }
                }
                if matches!(
                    data.kind,
                    ts_r_binder::ScopeKind::File | ts_r_binder::ScopeKind::Module
                ) && let Some(container) = data.symbol
                {
                    let exports = b.symbol(container).exports;
                    if exports != b.global_augmentations {
                        for (n, sym) in b.table(exports) {
                            let s = self.c.sym(e.file, *sym);
                            if !self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
                                && usable(self, s)
                            {
                                names.push(b.names.text(ast, *n).to_owned());
                            }
                        }
                    }
                }
            }
            scope = data.parent;
        }
        let mut globals: Vec<(ts_r_binder::program::SymbolRef, &str)> = Vec::new();
        for (g, merged) in self.c.link.globals.iter() {
            if let Some(&first) = merged.symbols.first()
                && let Some(s) = self.c.global_symbol(g, m)
                && !declaring(self, s)
            {
                globals.push((first, g));
            }
        }
        globals.sort();
        names.extend(globals.into_iter().map(|(_, g)| g.to_owned()));
        names
    }

    pub(crate) fn in_non_arrow_function(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => return true,
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// TS2448/2449/2450: block-scoped names used before their declaration in the same
    /// function body.
    fn check_use_before_declaration(&mut self, e: Loc, s: SymId, name: &str) {
        if !self.reporting(e.file) {
            return;
        }
        let f = self.c.sym_flags(s);
        if !f.intersects(
            SymbolFlags::BLOCK_SCOPED_VARIABLE | SymbolFlags::CLASS | SymbolFlags::REGULAR_ENUM,
        ) {
            return;
        }
        let Some(mut d) = self.c.value_decl(s).or_else(|| self.c.first_decl(s)) else {
            return;
        };
        // A namespace merged with a class or enum is not the declaration that matters.
        if matches!(self.node(d), Node::Module { .. }) {
            let other = self
                .c
                .decls(s)
                .into_iter()
                .find(|x| !matches!(self.node(*x), Node::Module { .. }));
            match other {
                Some(x) => d = x,
                None => return,
            }
        }
        if d.file != e.file || self.is_ambient_node(d) {
            return;
        }
        // `export = X` names the declaration rather than reading it.
        if self.c.parent(e).is_some_and(
            |p| matches!(self.node(p), Node::ExportAssignment { expr } if *expr == e.node),
        ) {
            return;
        }
        let use_span = self.c.span(e);
        let decl_span = self.c.span(d);
        // Inside its own initializer counts as before the declaration ends.
        let decl_container = self.declaration_container(d, f);
        // After a block-scoped name only reads in its own initializer are early (`const x =
        // x`, `[e = e]`, `let [a] = (() => a)()`).
        let block_scoped = f.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE);
        let in_own_init =
            block_scoped && use_span.start >= decl_span.start && self.is_in_own_initializer(e, d);
        if use_span.start >= decl_span.end && !in_own_init {
            return;
        }
        if block_scoped && use_span.start >= decl_span.start && !in_own_init {
            return;
        }
        // A use inside a function (or class member) declared in between is deferred.
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut prev = e.node;
        let mut cur = parents.parent(e.node);
        let mut param_decorator = false;
        while let Some(n) = cur {
            if Some(n) == decl_container {
                break;
            }
            match ast.node(n) {
                // Functions called where they are written run now. An async function or
                // generator does for later declarations (as tsc has it, past an `await` or
                // `yield` too), not for its own variable's initializer.
                Node::FunctionExpr { flags, .. } | Node::Arrow { flags, .. }
                    if !(in_own_init
                        && flags.intersects(
                            ts_r_ast::FnFlags::ASYNC | ts_r_ast::FnFlags::GENERATOR,
                        ))
                        && self.is_called_where_written(Loc::new(e.file, n)) => {}
                // A parameter's decorators run with its class.
                Node::Param { .. } if matches!(ast.node(prev), Node::Decorator { .. }) => {
                    param_decorator = true;
                }
                Node::FunctionExpr { .. } | Node::MethodDef { .. } if param_decorator => {}
                // An enum's members may read the enum itself.
                Node::Enum { .. } if n == d.node => return,
                // Computed member names run with the class, unless it is ambient.
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
                    if self.is_ambient_node(Loc::new(e.file, n)) =>
                {
                    return;
                }
                Node::MethodDef { key, .. } if *key == prev => {}
                Node::PropertyDef { key, modifiers, .. }
                    if *key == prev && !modifiers.contains(ts_r_ast::Modifiers::STATIC) => {}
                // Member decorators run with the class.
                Node::MethodDef { .. } | Node::PropertyDef { .. }
                    if matches!(ast.node(prev), Node::Decorator { .. }) => {}
                // Static blocks run with the class (its own binding exists by then).
                Node::StaticBlock { .. } => {
                    if f.intersects(SymbolFlags::CLASS) && parents.parent(n) == Some(d.node) {
                        return;
                    }
                }
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::PropertyDef { .. } => {
                    // Property initializers of non-static members and functions run later.
                    if let Node::PropertyDef {
                        modifiers, value, ..
                    } = *ast.node(n)
                        && modifiers.contains(ts_r_ast::Modifiers::STATIC)
                    {
                        // A class's own static initializers run once its binding exists
                        // (its computed names do not).
                        if value == Some(prev)
                            && f.intersects(SymbolFlags::CLASS)
                            && parents.parent(n) == Some(d.node)
                        {
                            return;
                        }
                        prev = n;
                        cur = parents.parent(n);
                        continue;
                    }
                    return;
                }
                // Type positions (computed names in interfaces and type literals) are not
                // evaluated.
                Node::TypeQuery { .. } | Node::Interface { .. } | Node::TypeLiteral { .. } => {
                    return;
                }
                // A class in its own heritage clause is circular (TS2506), not early.
                Node::HeritageClause { .. } if parents.parent(n) == Some(d.node) => return,
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        let info = if f.intersects(SymbolFlags::CLASS) {
            dg::CLASS_0_USED_BEFORE_ITS_DECLARATION_2449
        } else if f.intersects(SymbolFlags::REGULAR_ENUM) {
            dg::ENUM_0_USED_BEFORE_ITS_DECLARATION_2450
        } else {
            dg::BLOCK_SCOPED_VARIABLE_0_USED_BEFORE_ITS_DECLARATION_2448
        };
        self.error(e, info, &[name]);
    }

    fn declaration_container(&self, d: Loc, _f: SymbolFlags) -> Option<NodeId> {
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let mut cur = parents.parent(d.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::Block { .. }
                | Node::ModuleBlock { .. }
                | Node::Program { .. }
                | Node::For { .. }
                | Node::ForIn { .. }
                | Node::ForOf { .. }
                | Node::SwitchCase { .. }
                | Node::Switch { .. } => return Some(n),
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    /// Whether `e` is evaluated by an initializer of variable declaration `d`: the
    /// declarator's initializer or the default of a binding element holding the name,
    /// functions called where they are written included.
    fn is_in_own_initializer(&self, e: Loc, d: Loc) -> bool {
        let parents = self.c.file(e.file).parents();
        let ast = self.ast(e.file);
        let mut inits: Vec<NodeId> = Vec::new();
        let mut cur = Some(d.node);
        let mut declarator = None;
        for _ in 0..256 {
            let Some(n) = cur else {
                return false;
            };
            match *ast.node(n) {
                Node::VarDeclarator { init, .. } => {
                    inits.extend(init);
                    // A `for`-`in`/`of` variable's iterated expression is its initializer.
                    if let Some(head) = parents.parent(n).and_then(|l| parents.parent(l))
                        && let Node::ForIn { right, .. } | Node::ForOf { right, .. } =
                            *ast.node(head)
                    {
                        inits.push(right);
                    }
                    declarator = Some(n);
                    break;
                }
                Node::AssignPattern { right, .. } => inits.push(right),
                Node::BindingProperty { init, .. } => inits.extend(init),
                Node::Ident { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::Rest { .. } => {}
                _ => return false,
            }
            cur = parents.parent(n);
        }
        if declarator.is_none() || inits.is_empty() {
            return false;
        }
        let mut cur = Some(e.node);
        while let Some(n) = cur {
            if inits.contains(&n) {
                return true;
            }
            if cur == declarator {
                return false;
            }
            match ast.node(n) {
                Node::FunctionExpr { .. } | Node::Arrow { .. }
                    if !self.is_iife(Loc::new(e.file, n)) =>
                {
                    return false;
                }
                Node::ClassExpr { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// `this` in a function expression stored as a member: a property value of an object
    /// literal (the literal) or the value of `o.m = function () {}` (the type of `o`).
    fn assigned_function_this(&mut self, f: Loc, allow_self: bool) -> Option<TypeId> {
        let ast = self.ast(f.file);
        if !matches!(ast.node(f.node), Node::FunctionExpr { .. }) {
            return None;
        }
        let mut cur = f;
        for _ in 0..64 {
            let p = self.c.parent(cur)?;
            match *ast.node(p.node) {
                Node::Paren { .. } => cur = p,
                Node::Property { value, .. } if value == cur.node => {
                    let owner = self.c.parent(p)?;
                    if !matches!(ast.node(owner.node), Node::ObjectLit { .. }) {
                        return None;
                    }
                    return Some(self.object_literal_this(owner, allow_self));
                }
                Node::Assign { op, left, right }
                    if right == cur.node
                        // (`C.prototype.m ??= function` too.)
                        && matches!(
                            op,
                            ts_r_ast::AssignOp::Assign
                                | ts_r_ast::AssignOp::AndAssign
                                | ts_r_ast::AssignOp::OrAssign
                                | ts_r_ast::AssignOp::CoalesceAssign
                        ) =>
                {
                    let obj = match *ast.node(skip_parens(ast, left)) {
                        Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
                        _ => return None,
                    };
                    // JavaScript: `this` is the object (an instance for `C.prototype.m =
                    // function`), a silent `any` when its type isn't known (tsc 7 has no
                    // constructor functions). `exports.f = function` declares an export:
                    // its `this` is untyped (but `module.exports.f = function` has the
                    // module's `this`).
                    if self.c.file(f.file).is_js() {
                        if is_exports_identifier(ast, skip_parens(ast, obj)) {
                            return None;
                        }
                        let t = self.quietly(|ck| ck.check_expr(Loc::new(f.file, obj)));
                        let known = t != TypeId::UNRESOLVED
                            && !self.store().is_error(t)
                            && self.store().flags(t).intersects(TypeFlags::OBJECT)
                            && !self.is_empty_object_type(t);
                        return Some(if known { t } else { TypeId::ANY });
                    }
                    let t = self.quietly(|ck| ck.check_expr(Loc::new(f.file, obj)));
                    if t == TypeId::UNRESOLVED || self.store().is_error(t) {
                        return Some(TypeId::ANY);
                    }
                    let t = self.widen_literal(t);
                    return Some(self.widened_type(t));
                }
                _ => return None,
            }
        }
        None
    }

    /// Whether `this` or `super` at `e` is referenced from a computed property name
    /// (TS2465/TS2466). Observed:
    /// - `this`: a class member's computed name counts; an object literal member's is
    ///   skipped. Non-arrow functions and classes end the search.
    /// - `super`: a class member's computed name counts unless the class itself sits in a
    ///   method, accessor, constructor, field or static block (that `super` is the outer
    ///   one); an object literal member's is skipped, except that a `super(...)` call in
    ///   one inside an arrow function counts.
    fn in_computed_name(&self, e: Loc, is_super: bool) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let super_call = is_super
            && parents.parent(e.node).is_some_and(
                |p| matches!(*ast.node(p), Node::Call { callee, .. } if callee == e.node),
            );
        let mut in_object_key = false;
        let mut in_class_key = false;
        let mut prev = e.node;
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::ComputedKey { .. } => {
                    let in_class = parents
                        .parent(n)
                        .and_then(|m| parents.parent(m))
                        .is_some_and(|c| {
                            matches!(ast.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                        });
                    if in_class && !is_super {
                        return true;
                    }
                    if in_class {
                        in_class_key = true;
                    } else {
                        in_object_key = true;
                    }
                }
                // (`this` and `super` in an arrow are the enclosing ones.)
                Node::Arrow { .. } if super_call && in_object_key => return true,
                Node::ClassDecl { .. } | Node::ClassExpr { .. } if is_super && in_class_key => {}
                // An outer `super` container: a member body, a field value or a static block.
                Node::FunctionExpr { .. }
                    if is_super
                        && parents
                            .parent(n)
                            .is_some_and(|m| matches!(ast.node(m), Node::MethodDef { .. })) =>
                {
                    return false;
                }
                Node::PropertyDef { value: Some(v), .. } if is_super && v == prev => return false,
                Node::StaticBlock { .. } if is_super => return false,
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::StaticBlock { .. }
                | Node::Module { .. }
                | Node::Program { .. } => return is_super && in_class_key,
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        is_super && in_class_key
    }

    pub(crate) fn check_this(&mut self, e: Loc) -> TypeId {
        if self.in_computed_name(e, false) {
            self.error(
                e,
                dg::THIS_CANNOT_BE_REFERENCED_IN_A_COMPUTED_PROPERTY_NAME_2465,
                &[],
            );
        }
        self.check_super_called_before(e, false);
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        let mut prev = e.node;
        let mut guard = 0;
        // `this` in a computed name (`[this.a]: 1`) never falls back to the literal's type.
        let mut in_key = false;
        let mut in_arrow = false;
        while let Some(n) = cur {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            match *ast.node(n) {
                Node::ComputedKey { .. } => in_key = true,
                Node::Arrow { .. } => in_arrow = true,
                Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => {
                    // An explicit `this` parameter.
                    for &p in ast.list(params) {
                        if crate::nodes::is_this_param(ast, p)
                            && let Node::Param {
                                type_ann: Some(t), ..
                            } = *ast.node(p)
                        {
                            // (Narrowed like any reference: `if (typeof this === "string")`.)
                            let t = self.type_from_node(Loc::new(e.file, t));
                            return self.flow_type_of_reference(e, t, t);
                        }
                    }
                    // Methods of classes and object literals.
                    if let Some(m) = parents.parent(n)
                        && let Node::MethodDef { modifiers, .. } = *ast.node(m)
                        && let Some(owner) = parents.parent(m)
                    {
                        match ast.node(owner) {
                            Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                                return self.class_this(
                                    Loc::new(e.file, owner),
                                    modifiers.contains(ts_r_ast::Modifiers::STATIC),
                                    e,
                                );
                            }
                            Node::ObjectLit { .. } => {
                                // A `this` parameter of the contextual signature comes first.
                                if let Some(ctx) = self.contextual_signature(Loc::new(e.file, n))
                                    && let Some(t) = self.store().sig(ctx).this_type
                                {
                                    return t;
                                }
                                return self.object_literal_this(Loc::new(e.file, owner), !in_key);
                            }
                            _ => {}
                        }
                    }
                    if let Some(ctx) = self.contextual_signature(Loc::new(e.file, n))
                        && let Some(t) = self.store().sig(ctx).this_type
                    {
                        return t;
                    }
                    if let Some(t) = self.assigned_function_this(Loc::new(e.file, n), !in_key) {
                        return t;
                    }
                    if self.c.opts.no_implicit_this
                        && matches!(
                            ast.node(n),
                            Node::FunctionDecl { .. } | Node::FunctionExpr { .. }
                        )
                    {
                        self.error(e, dg::THIS_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_IT_DOES_NOT_HAVE_A_TYPE_ANNOTATION_2683, &[]);
                    }
                    return TypeId::ANY;
                }
                Node::PropertyDef { modifiers, .. } | Node::MethodDef { modifiers, .. } => {
                    if let Some(owner) = parents.parent(n)
                        && matches!(
                            ast.node(owner),
                            Node::ClassDecl { .. } | Node::ClassExpr { .. }
                        )
                    {
                        // TS2816: a static field value of a class with legacy decorators.
                        if let Node::PropertyDef {
                            value: Some(v),
                            modifiers,
                            ..
                        } = *ast.node(n)
                            && v == prev
                            && modifiers.contains(ts_r_ast::Modifiers::STATIC)
                            && self.c.opts.experimental_decorators
                            && let Node::ClassDecl { decorators, .. }
                            | Node::ClassExpr { decorators, .. } = *ast.node(owner)
                            && !decorators.is_empty()
                        {
                            self.error(
                                e,
                                dg::CANNOT_USE_THIS_IN_A_STATIC_PROPERTY_INITIALIZER_OF_A_DECORATED_CLASS_2816,
                                &[],
                            );
                        }
                        return self.class_this(
                            Loc::new(e.file, owner),
                            modifiers.contains(ts_r_ast::Modifiers::STATIC),
                            e,
                        );
                    }
                }
                Node::StaticBlock { .. } => {
                    if let Some(owner) = parents.parent(n) {
                        return self.class_this(Loc::new(e.file, owner), true, e);
                    }
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    // Computed names and heritage clauses: the outer `this`.
                }
                // A namespace body (TS2331) and an enum member (TS2332) have no `this`: it is
                // `any`, implicitly so under noImplicitThis.
                Node::Module { .. } | Node::Enum { .. } => {
                    if self.reporting(e.file) {
                        let d = if matches!(ast.node(n), Node::Module { .. }) {
                            dg::THIS_CANNOT_BE_REFERENCED_IN_A_MODULE_OR_NAMESPACE_BODY_2331
                        } else {
                            dg::THIS_CANNOT_BE_REFERENCED_IN_CURRENT_LOCATION_2332
                        };
                        self.error(e, d, &[]);
                        if self.c.opts.no_implicit_this {
                            self.error(e, dg::THIS_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_IT_DOES_NOT_HAVE_A_TYPE_ANNOTATION_2683, &[]);
                        }
                    }
                    return TypeId::ANY;
                }
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        // A script's top-level `this` is the global object, a module's is `undefined`.
        if !self.c.file(e.file).is_js() {
            if !self.c.file(e.file).bound.is_module {
                // An arrow function there captures it, implicitly (TS7041).
                if in_arrow && self.c.opts.no_implicit_this {
                    self.error(
                        e,
                        dg::THE_CONTAINING_ARROW_FUNCTION_CAPTURES_THE_GLOBAL_VALUE_OF_THIS_7041,
                        &[],
                    );
                }
                return self.global_this_type();
            }
            // Observed: `this.x` at the top level of a module, or in an arrow there, is
            // TS2532 (not in declaration files).
            if !self.c.file(e.file).is_declaration() {
                return TypeId::UNDEFINED;
            }
        }
        TypeId::UNRESOLVED
    }

    /// `this` in a method of the object literal `ol`: the argument of a `ThisType<X>` marker
    /// in the contextual type of `ol` or of an object literal it is a property value of,
    /// else the non-nullable contextual type of `ol`, else (under `noImplicitThis` or in
    /// JavaScript, when `allow_self` and `ol` is in a position nothing contextually types)
    /// the widened type of `ol` itself (`any` while that type is being computed).
    fn object_literal_this(&mut self, ol: Loc, allow_self: bool) -> TypeId {
        let mut lit = ol;
        for _ in 0..64 {
            if let Some(ct) = self.contextual_type(lit)
                && let Some(x) = self.this_type_marker_argument(ct, 0)
            {
                return x;
            }
            let parents = self.c.file(lit.file).parents();
            let Some(p) = parents.parent(lit.node) else {
                break;
            };
            let ast = self.ast(lit.file);
            match *ast.node(p) {
                Node::Property { value, .. } if value == lit.node => {}
                _ => break,
            }
            match parents.parent(p) {
                Some(o) if matches!(ast.node(o), Node::ObjectLit { .. }) => {
                    lit = Loc::new(lit.file, o)
                }
                _ => break,
            }
        }
        match self.contextual_type(ol) {
            Some(t) if self.c.opts.strict_null_checks => self.non_nullable(t),
            Some(t) => t,
            None => {
                if !allow_self
                    || !(self.c.opts.no_implicit_this || self.c.file(ol.file).is_js())
                    || !self.is_uncontextual_literal(ol)
                    || !self.push_resolution(crate::checker::Resolving::Node(ol))
                {
                    return TypeId::UNRESOLVED;
                }
                let t = self.quietly(|ck| ck.check_expr(ol));
                self.pop_resolution();
                if t == TypeId::UNRESOLVED || self.store().is_error(t) {
                    return TypeId::UNRESOLVED;
                }
                self.widened_type(t)
            }
        }
    }

    /// Whether the object literal `ol` (or the literal it is a property value of) is the
    /// initializer of an unannotated variable, the right side of an assignment, an
    /// expression statement or an `export default`: places with no contextual type.
    fn is_uncontextual_literal(&self, ol: Loc) -> bool {
        let ast = self.ast(ol.file);
        let parents = self.c.file(ol.file).parents();
        let mut cur = ol.node;
        for _ in 0..256 {
            let Some(p) = parents.parent(cur) else {
                return false;
            };
            match *ast.node(p) {
                Node::Paren { .. } => cur = p,
                Node::Property { value, .. } if value == cur => match parents.parent(p) {
                    Some(o) if matches!(ast.node(o), Node::ObjectLit { .. }) => cur = o,
                    _ => return false,
                },
                Node::VarDeclarator { init, type_ann, .. } => {
                    return init == Some(cur) && type_ann.is_none();
                }
                Node::Assign { right, .. } => return right == cur,
                Node::ExprStmt { .. } | Node::ExportDefaultExpr { .. } => return true,
                _ => return false,
            }
        }
        false
    }

    /// Whether the contextual type of the template expression `e` has a string literal or
    /// template literal member, or a generic member constrained to a string-like type.
    fn is_template_literal_context(&mut self, e: Loc) -> bool {
        // An element access index or a computed property name (through parentheses) keeps
        // the literal: `o[`x${c}`]` reads property `x1`.
        let mut cur = e;
        while let Some(p) = self.c.parent(cur) {
            match *self.node(p) {
                Node::Paren { .. } => cur = p,
                Node::Index { index, .. } if index == cur.node => return true,
                Node::ComputedKey { .. } => return true,
                _ => break,
            }
        }
        let Some(ct) = self.contextual_type(e) else {
            return false;
        };
        let members = self.union_members(ct);
        members.into_iter().any(|m| {
            let f = self.store().flags(m);
            if f.intersects(TypeFlags::STRING_LITERAL | TypeFlags::TEMPLATE_LITERAL) {
                return true;
            }
            if f.intersects(TypeFlags::INSTANTIABLE_NON_PRIMITIVE) {
                let b = self.base_constraint(m);
                return b != TypeId::UNRESOLVED
                    && self
                        .union_members(b)
                        .into_iter()
                        .any(|x| self.store().flags(x).intersects(TypeFlags::STRING_LIKE));
            }
            false
        })
    }

    /// `X` when `t` is (or is an intersection containing) the global `ThisType<X>`; for a
    /// union, the union of its members' markers.
    fn this_type_marker_argument(&mut self, t: TypeId, depth: u32) -> Option<TypeId> {
        if depth > 8 {
            return None;
        }
        let s = self.store();
        if s.flags(t).contains(TypeFlags::INTERSECTION) {
            for m in s.members_of_union(t).to_vec() {
                if let Some(x) = self.this_type_marker_argument(m, depth + 1) {
                    return Some(x);
                }
            }
            return None;
        }
        // A union (`Opts<D> | undefined` from an optional parameter): its members' markers.
        if s.flags(t).contains(TypeFlags::UNION) {
            let mut found = Vec::new();
            for m in s.members_of_union(t).to_vec() {
                if let Some(x) = self.this_type_marker_argument(m, depth + 1) {
                    found.push(x);
                }
            }
            return (!found.is_empty()).then(|| self.union(&found));
        }
        let Some(ts_r_types::ObjectKind::Reference { target, args }) =
            s.object_of(t).map(|o| o.kind)
        else {
            return None;
        };
        let sym = self.c.global_symbol("ThisType", meaning::TYPE)?;
        if self.declared_type_of_symbol(sym) != target {
            return None;
        }
        s.list_items(args).first().copied()
    }

    fn class_this(&mut self, class: Loc, is_static: bool, e: Loc) -> TypeId {
        let Some(s) = self.c.sym_of_decl(class) else {
            return TypeId::UNRESOLVED;
        };
        if is_static {
            return self.type_of_symbol(s);
        }
        let t = self.store().type_param(s, true);
        self.flow_type_of_reference(e, t, t)
    }

    fn check_super(&mut self, e: Loc) -> TypeId {
        if self.in_computed_name(e, true) {
            self.error(
                e,
                dg::SUPER_CANNOT_BE_REFERENCED_IN_A_COMPUTED_PROPERTY_NAME_2466,
                &[],
            );
        }
        if let Some(info) = self.super_placement_error(e) {
            if self.reporting(e.file) {
                self.error(e, info, &[]);
            }
            if info.code != 2336 {
                return TypeId::UNRESOLVED;
            }
        }
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        // `super.x` (or a bare `super`), not `super(...)`.
        if !parents
            .parent(e.node)
            .is_some_and(|p| matches!(*ast.node(p), Node::Call { callee, .. } if callee == e.node))
        {
            self.check_super_called_before(e, true);
        }
        let mut cur = parents.parent(e.node);
        let mut is_static = false;
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::MethodDef { modifiers, .. } | Node::PropertyDef { modifiers, .. } => {
                    is_static = modifiers.contains(ts_r_ast::Modifiers::STATIC);
                }
                Node::StaticBlock { .. } => is_static = true,
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    let Some(s) = self.c.sym_of_decl(Loc::new(e.file, n)) else {
                        return TypeId::UNRESOLVED;
                    };
                    if is_static {
                        return self.base_constructor_type(s).unwrap_or(TypeId::UNRESOLVED);
                    }
                    let bases = self.base_types(s);
                    return bases.first().copied().unwrap_or(TypeId::UNRESOLVED);
                }
                Node::ObjectLit { .. } => return TypeId::UNRESOLVED,
                _ => {}
            }
            cur = parents.parent(n);
        }
        TypeId::UNRESOLVED
    }

    // ---------------------------------------------------------------------------------
    // Null checks
    // ---------------------------------------------------------------------------------

    /// tsc's `checkNonNullType`: reports possibly-null/undefined operands and returns the
    /// type without `null`/`undefined`.
    pub fn check_non_null(&mut self, node: Loc, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            // Without strictNullChecks only a value that is nothing but `undefined` or
            // `null` is reported (observed: `u.x` with `u: undefined`, module-level
            // `this.x`, `null.x`).
            let (undef, null) = match t {
                TypeId::UNDEFINED | TypeId::UNDEFINED_WIDENING => (true, false),
                TypeId::NULL | TypeId::NULL_WIDENING => (false, true),
                _ => return t,
            };
            self.report_possibly_nullish(node, undef, null);
            return TypeId::ERROR;
        }
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNKNOWN) {
            let ast = self.ast(node.file);
            let n = skip_parens(ast, node.node);
            if is_entity_name_expression(ast, n) {
                let text = entity_text(ast, n);
                self.error(node, dg::_0_IS_OF_TYPE_UNKNOWN_18046, &[&text]);
            } else {
                self.error(node, dg::OBJECT_IS_OF_TYPE_UNKNOWN_2571, &[]);
            }
            return TypeId::ERROR;
        }
        // `void` is not reported as possibly `undefined` (it stays, and fails later checks).
        let (has_undef, has_null) = self.nullable_facts(t);
        let void_only = has_undef
            && !self.some_member(t, |m| self.store().flags(m).contains(TypeFlags::UNDEFINED));
        if !has_null && (!has_undef || void_only) {
            return t;
        }
        let has_undef = has_undef && !void_only;
        self.report_possibly_nullish(node, has_undef, has_null);
        let nn = self.non_nullable_strict(t);
        if self
            .store()
            .flags(nn)
            .intersects(TypeFlags::NULLABLE | TypeFlags::NEVER)
        {
            return TypeId::ERROR;
        }
        nn
    }

    /// `(may be undefined, may be null)`.
    pub fn nullable_facts(&self, t: TypeId) -> (bool, bool) {
        let s = self.store();
        let mut u = false;
        let mut n = false;
        let mut each = |m: TypeId| {
            let f = s.flags(m);
            if f.intersects(TypeFlags::UNDEFINED | TypeFlags::VOID) {
                u = true;
            }
            if f.contains(TypeFlags::NULL) {
                n = true;
            }
            // `T & null` (a type parameter narrowed by `typeof x === "object"`).
            if f.contains(TypeFlags::INTERSECTION) {
                for &p in s.members_of_union(m) {
                    let pf = s.flags(p);
                    if pf.contains(TypeFlags::UNDEFINED) {
                        u = true;
                    }
                    if pf.contains(TypeFlags::NULL) {
                        n = true;
                    }
                }
            }
        };
        if s.flags(t).contains(TypeFlags::UNION) {
            for &m in s.members_of_union(t) {
                each(m);
            }
        } else {
            each(t);
        }
        (u, n)
    }

    /// `x!` of a type parameter whose constraint may be `null` or `undefined`, member by member
    /// in a union: where the contextual type has no type parameters (an argument for a
    /// `string` parameter) the constraint without `null` and `undefined` (`T extends string |
    /// undefined` gives `string`), elsewhere `T & {}`.
    fn non_null_type_params(&mut self, e: Loc, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks
            || !self.some_member(t, |m| {
                self.store().flags(m).contains(TypeFlags::TYPE_PARAMETER)
            })
        {
            return t;
        }
        let ct = match self.contextual_type(e) {
            Some(ct) => Some(ct),
            None => self.plain_argument_context(e),
        };
        let substitute = ct.is_some_and(|ct| !self.is_generic_type(ct));
        let members = self.union_members(t);
        let mut out = Vec::with_capacity(members.len());
        let mut changed = false;
        for m in members {
            if self.store().flags(m).contains(TypeFlags::TYPE_PARAMETER) {
                let c = self.base_constraint(m);
                if c != TypeId::UNRESOLVED {
                    let (u, n) = self.nullable_facts(c);
                    if u || n || c == TypeId::UNKNOWN {
                        let r = if substitute && c != TypeId::UNKNOWN {
                            self.non_nullable(c)
                        } else {
                            self.non_nullable_of_generic(m)
                        };
                        out.push(r);
                        changed = true;
                        continue;
                    }
                }
            }
            out.push(m);
        }
        if changed { self.union(&out) } else { t }
    }

    /// The parameter type for argument `e` of a call whose callee has a single non-generic
    /// signature (known before the call is resolved).
    fn plain_argument_context(&mut self, e: Loc) -> Option<TypeId> {
        let parent = self.c.parent(e)?;
        let ast = self.ast(e.file);
        let (Node::Call { callee, args, .. } | Node::New { callee, args, .. }) =
            *ast.node(parent.node)
        else {
            return None;
        };
        let idx = ast.list(args).iter().position(|&a| a == e.node)?;
        let kind = if matches!(ast.node(parent.node), Node::New { .. }) {
            crate::members::SigKind::Construct
        } else {
            crate::members::SigKind::Call
        };
        let ft = self.quietly(|ck| ck.check_expr(Loc::new(e.file, callee)));
        let sigs = self.signatures_of(ft, kind);
        let [sig] = sigs[..] else {
            return None;
        };
        if self.store().sig(sig).type_params != ts_r_types::ListId::EMPTY {
            return None;
        }
        self.param_type_at(sig, idx)
    }

    pub(crate) fn non_nullable_strict(&mut self, t: TypeId) -> TypeId {
        self.filter_type(t, |ck, m| !ck.is_nullish_member(m))
    }

    pub(crate) fn report_possibly_nullish(&mut self, node: Loc, undef: bool, null: bool) {
        let ast = self.ast(node.file);
        let n = skip_parens(ast, node.node);
        // (A parenthesized `(null)` is an object possibly null, observed.)
        if let Node::NullLit {} = ast.node(node.node) {
            self.error(node, dg::THE_VALUE_0_CANNOT_BE_USED_HERE_18050, &["null"]);
            return;
        }
        // `this.x` is no entity name here: TS2532 (tsc 7).
        let mut root = n;
        while let Node::Member { obj, .. } = *ast.node(root) {
            root = obj;
        }
        if is_entity_name_expression(ast, n) && !matches!(ast.node(root), Node::This {}) {
            let text = entity_text(ast, n);
            if text.len() < 100 {
                if text == "undefined" {
                    self.error(
                        node,
                        dg::THE_VALUE_0_CANNOT_BE_USED_HERE_18050,
                        &["undefined"],
                    );
                    return;
                }
                let info = match (undef, null) {
                    (true, true) => dg::_0_IS_POSSIBLY_NULL_OR_UNDEFINED_18049,
                    (true, false) => dg::_0_IS_POSSIBLY_UNDEFINED_18048,
                    _ => dg::_0_IS_POSSIBLY_NULL_18047,
                };
                self.error(node, info, &[&text]);
                return;
            }
        }
        let info = match (undef, null) {
            (true, true) => dg::OBJECT_IS_POSSIBLY_NULL_OR_UNDEFINED_2533,
            (true, false) => dg::OBJECT_IS_POSSIBLY_UNDEFINED_2532,
            _ => dg::OBJECT_IS_POSSIBLY_NULL_2531,
        };
        self.error(node, info, &[]);
    }

    // ---------------------------------------------------------------------------------
    // Optional chains, member and element access
    // ---------------------------------------------------------------------------------

    /// Whether `e` (a member, element access or call) is part of an optional chain.
    pub fn in_optional_chain(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let mut n = e.node;
        for _ in 0..10_000 {
            match *ast.node(n) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional {
                        return true;
                    }
                    n = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    if optional {
                        return true;
                    }
                    n = callee;
                }
                Node::NonNull { expr } => n = expr,
                _ => return false,
            }
        }
        false
    }

    fn add_chain_undefined(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        self.union2(t, TypeId::UNDEFINED)
    }

    /// The type of a chain link without the `undefined` its short-circuit adds, and whether
    /// some link short-circuits.
    pub fn check_chain(&mut self, e: Loc) -> (TypeId, bool) {
        let ast = self.ast(e.file);
        let f = e.file;
        match *ast.node(e.node) {
            Node::Member {
                obj,
                optional,
                prop,
            } => {
                if let Some(t) = self.commonjs_module_exports_type(e) {
                    return (t, false);
                }
                let (ot, short) = self.chain_object(Loc::new(f, obj));
                let (ot, strips) = self.optional_link(ot, optional);
                if optional && self.store().flags(ot).contains(TypeFlags::NEVER) {
                    return (TypeId::NEVER, true);
                }
                (
                    self.property_access(e, obj, ot, prop, false),
                    short || strips,
                )
            }
            Node::Index {
                obj,
                optional,
                index,
            } => {
                let (ot, short) = self.chain_object(Loc::new(f, obj));
                let (ot, strips) = self.optional_link(ot, optional);
                (
                    self.element_access(e, obj, ot, index, false),
                    short || strips,
                )
            }
            Node::Call {
                callee, optional, ..
            } => {
                let (ct, short) = self.chain_object(Loc::new(f, callee));
                let (ct, strips) = self.optional_link(ct, optional);
                (self.check_call(e, Some(ct)), short || strips)
            }
            Node::NonNull { expr } => {
                let (t, short) = self.chain_object(Loc::new(f, expr));
                (self.non_nullable(t), short)
            }
            _ => (self.check_expr(e), false),
        }
    }

    /// The object type an optional (`?.`) link reads from, and whether the link can short-circuit:
    /// only when the object type may be `null` or `undefined` does the chain's result gain
    /// `undefined` (`one?.price` is `number` for a non-nullable `one`, as in tsc).
    fn optional_link(&mut self, ot: TypeId, optional: bool) -> (TypeId, bool) {
        if !optional {
            return (ot, false);
        }
        // `unknown` past the link is `{}` (`u?.toString()` is no TS18046; observed).
        let nn = if ot == TypeId::UNKNOWN && self.c.opts.strict_null_checks {
            TypeId::EMPTY_OBJECT
        } else {
            self.non_nullable_strict(ot)
        };
        let strips = nn != ot || self.may_be_nullish(ot, 0);
        // A type variable that may be nullish is `NonNullable<T>` past the link
        // (`t?.toString()` on an unconstrained `T` reads `T & {}`).
        let nn = self.non_nullable_generic_members(nn);
        (nn, strips)
    }

    /// Whether `t` may hold `null` or `undefined`: `any`, `unknown`, or a type variable whose
    /// base constraint may (an unconstrained one may).
    fn may_be_nullish(&mut self, t: TypeId, depth: u32) -> bool {
        let members: Vec<TypeId> = if self.store().flags(t).contains(TypeFlags::UNION) {
            self.store().members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        members.into_iter().any(|m| {
            let f = self.store().flags(m);
            if f.intersects(TypeFlags::ANY | TypeFlags::UNKNOWN) || self.is_nullish_member(m) {
                return true;
            }
            if f.intersects(TypeFlags::INSTANTIABLE) && depth < 8 {
                let c = self.base_constraint(m);
                return c != m && self.may_be_nullish(c, depth + 1);
            }
            false
        })
    }

    fn chain_object(&mut self, obj: Loc) -> (TypeId, bool) {
        let ast = self.ast(obj.file);
        match ast.node(obj.node) {
            Node::Member { .. } | Node::Index { .. } | Node::Call { .. } | Node::NonNull { .. }
                if self.in_optional_chain(obj) =>
            {
                self.check_chain(obj)
            }
            _ => (self.check_expr(obj), false),
        }
    }

    /// `obj.prop` with `ot` the type of `obj`.
    pub fn property_access(
        &mut self,
        e: Loc,
        obj: NodeId,
        ot: TypeId,
        prop: NodeId,
        is_write: bool,
    ) -> TypeId {
        let ast = self.ast(e.file);
        let name = match *ast.node(prop) {
            Node::Ident { name } => ast.atom(name).to_owned(),
            Node::PrivateName { name } => format!("#{}", ast.atom(name)),
            _ => return TypeId::ERROR,
        };
        // Members emptied by conflicting discriminants are gone (`reduced_type`); an
        // intersection emptied so says why.
        let orig = ot;
        let ot = self.reduced_type(ot);
        if ot == TypeId::NEVER
            && orig != ot
            && let Some((info, os, pname)) = self.never_reduction_reason(orig)
        {
            if self.reporting(e.file) {
                let mut chain = Chain::new();
                chain.push_back(
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&name, "never"],
                );
                chain.push_back(info, &[&os, &pname]);
                self.error_chain(Loc::new(e.file, prop), &chain);
            }
            return TypeId::ERROR;
        }
        let ol = Loc::new(e.file, obj);
        // `anyValue.#x` outside every class body: TS18016 at the name (tsc 7).
        if name.starts_with('#')
            && ot != TypeId::UNRESOLVED
            && (self.store().is_error(ot) || self.store().flags(ot).intersects(TypeFlags::ANY))
            && self.enclosing_class(e).is_none()
            && self.reporting(e.file)
        {
            self.error(
                Loc::new(e.file, prop),
                dg::PRIVATE_IDENTIFIERS_ARE_NOT_ALLOWED_OUTSIDE_CLASS_BODIES_18016,
                &[],
            );
        }
        if ot == TypeId::UNRESOLVED || ot == TypeId::SILENT_NEVER || self.store().is_error(ot) {
            return ot;
        }
        // `anyValue.#x` in a class body where no enclosing class declares `#x` (observed:
        // TS2339 on `any`, also after `unknown`'s TS18046).
        let undeclared_private = |ck: &mut Self| {
            name.starts_with('#')
                && ck.enclosing_class(e).is_some()
                && !ck.private_name_in_scope(e, &name)
                && ck.reporting(e.file)
        };
        if self.store().flags(ot).intersects(TypeFlags::ANY) {
            if undeclared_private(self) {
                self.error(
                    Loc::new(e.file, prop),
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&name, "any"],
                );
            }
            return TypeId::ANY;
        }
        let is_super = matches!(ast.node(skip_parens(ast, obj)), Node::Super {});
        let nt = self.check_non_null(ol, ot);
        if self.store().is_error(nt) {
            if ot == TypeId::UNKNOWN && undeclared_private(self) {
                self.error(
                    Loc::new(e.file, prop),
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&name, "any"],
                );
            }
            return TypeId::ERROR;
        }
        let pl = Loc::new(e.file, prop);
        let name_id = self.store().intern_str(&name);
        let apparent = self.apparent_type(nt);
        if apparent == TypeId::UNRESOLVED {
            return TypeId::UNRESOLVED;
        }
        // A type parameter constrained to `any` (or an unresolved import's error type):
        // the access is `any`, as tsc gives it, so implicit-any checks still run downstream.
        if self.store().flags(apparent).intersects(TypeFlags::ANY) {
            return TypeId::ANY;
        }
        let found = self.property_of_type_id(nt, name_id);
        // A static `#private` field belongs to its class's constructor only: a derived
        // class's `typeof Child` does not have it (observed: TS2339).
        if let Some(p) = found
            && name.starts_with('#')
            && let Some(d) = self.c.value_decl(p)
            && self
                .node(d)
                .modifiers()
                .contains(ts_r_ast::Modifiers::STATIC)
            && let Some(dc) = self.enclosing_class(d)
            && let Some(ts_r_types::ObjectKind::Anonymous { decl, kind, .. }) =
                self.store().object_of(nt).map(|o| o.kind)
            && kind == crate::declared::anon::CLASS_STATIC
            && let Some(dsym) = self.c.sym_of_decl(dc)
            && decl != crate::declared::sym_key(dsym)
        {
            self.report_missing_property(pl, &name, nt);
            return TypeId::ERROR;
        }
        if let Some(p) = found {
            self.check_property_accessibility(pl, p, nt, &name, is_super);
            self.check_property_initialization_order(e, obj, pl, p, &name, is_write);
            self.check_abstract_this_access(e, obj, pl, p, &name);
            if is_write && self.is_readonly_write(e, p) {
                self.error(
                    pl,
                    dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_READ_ONLY_PROPERTY_2540,
                    &[&name],
                );
                return TypeId::ERROR;
            }
            let t = self.type_of_symbol(p);
            if !is_write && self.used_before_assignment_declaration(e, obj, p, t) {
                self.error(
                    pl,
                    dg::PROPERTY_0_IS_USED_BEFORE_BEING_ASSIGNED_2565,
                    &[&name],
                );
                return t;
            }
            if is_write {
                // A setter's declared parameter type is what a write must satisfy.
                if let Some(w) = self.setter_type(p) {
                    return w;
                }
                if let Some(ps) = self.late_bound_setter(p)
                    && let Some(w) = self.setter_type(ps)
                {
                    return w;
                }
                // Writes to an optional property don't accept `undefined` under
                // `exactOptionalPropertyTypes`.
                let wt = self.write_type_of_symbol(p);
                return self.write_type_of_property(wt);
            }
            let pf = self.c.sym_flags(p);
            if (pf
                .intersects(SymbolFlags::PROPERTY | SymbolFlags::ACCESSOR | SymbolFlags::VARIABLE)
                || self.c.synth_sym(p).is_some())
                && self.is_narrowable_reference(e)
            {
                return self.flow_type_of_reference(e, t, t);
            }
            return t;
        }
        // Index signatures.
        if !name.starts_with('#') {
            let numeric = is_numeric_name(&name);
            let info = if numeric {
                self.index_info_of(nt, TypeId::NUMBER)
            } else {
                self.index_info_of(nt, TypeId::STRING)
            };
            if let Some(i) = info {
                if self.c.opts.no_property_access_from_index_signature && !is_write {
                    self.error(pl, dg::PROPERTY_0_COMES_FROM_AN_INDEX_SIGNATURE_SO_IT_MUST_BE_ACCESSED_WITH_0_4111, &[&name]);
                }
                if is_write && i.readonly {
                    let ts = self.type_to_string(nt);
                    self.error(
                        e,
                        dg::INDEX_SIGNATURE_IN_TYPE_0_ONLY_PERMITS_READING_2542,
                        &[&ts],
                    );
                    // The assigned value is still checked against the signature's type.
                    return i.value;
                }
                let mut t = i.value;
                if self.c.opts.no_unchecked_indexed_access && !is_write {
                    t = self.add_undefined(t);
                }
                if !is_write && self.is_narrowable_reference(e) {
                    return self.flow_type_of_reference(e, t, t);
                }
                return t;
            }
        }
        // An object literal of a JavaScript file is open without `noImplicitAny`: a missing
        // property is `any`.
        if !self.c.opts.no_implicit_any && self.is_js_literal_type(apparent) {
            return TypeId::ANY;
        }
        // A missing global through `globalThis` is implicitly `any` (TS7017 under
        // noImplicitAny), unless it names a global `let`/`const` (not a property of the
        // global object): that is a missing property.
        if !name.starts_with('#')
            && nt == self.global_this_type()
            && !self
                .c
                .global_symbol(&name, meaning::VALUE)
                .is_some_and(|g| {
                    self.c
                        .sym_flags(g)
                        .intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                })
        {
            if self.c.opts.no_implicit_any {
                let ts = self.type_to_string(nt);
                self.error(
                    pl,
                    dg::ELEMENT_IMPLICITLY_HAS_AN_ANY_TYPE_BECAUSE_TYPE_0_HAS_NO_INDEX_SIGNATURE_7017,
                    &[&ts],
                );
            }
            return TypeId::ANY;
        }
        self.report_missing_property(pl, &name, nt);
        TypeId::ERROR
    }

    /// A write of property `p` through access `e` is to a read-only property: one declared
    /// read-only (outside the constructor that may initialize it), or any member of a
    /// namespace import (`import * as ns`; `ns.x = 1` is TS2540, tsc 7).
    fn is_readonly_write(&mut self, e: Loc, p: SymId) -> bool {
        (self.is_readonly_symbol(p)
            && !self.is_readonly_write_allowed(e, p)
            && self.late_bound_setter(p).is_none())
            || self.through_namespace_import(e)
    }

    /// The setter paired with getter `p` when `p` has a computed name (`get [k]()`) and
    /// its members also hold `set [k](v)`: the pair is one writable property.
    fn late_bound_setter(&mut self, p: SymId) -> Option<SymId> {
        let d = self.c.value_decl(p)?;
        let ast = self.ast(d.file);
        let (Node::MethodDef {
            kind: MethodKind::Get,
            key,
            ..
        }
        | Node::MethodSignature {
            kind: MethodKind::Get,
            key,
            ..
        }) = *ast.node(d.node)
        else {
            return None;
        };
        let Node::ComputedKey { expr } = *ast.node(key) else {
            return None;
        };
        let name = self.late_bound_name(Loc::new(d.file, expr))?;
        let parent = self.c.file(d.file).parents().parent(d.node)?;
        let siblings = match *ast.node(parent) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            Node::Interface { body, .. } => body,
            Node::TypeLiteral { members } => members,
            _ => return None,
        };
        let setters: Vec<(NodeId, NodeId)> = ast
            .list(siblings)
            .iter()
            .filter_map(|&m| match *ast.node(m) {
                Node::MethodDef {
                    kind: MethodKind::Set,
                    key,
                    ..
                }
                | Node::MethodSignature {
                    kind: MethodKind::Set,
                    key,
                    ..
                } => match *ast.node(key) {
                    Node::ComputedKey { expr } => Some((m, expr)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        setters
            .into_iter()
            .find(|&(_, x)| {
                self.late_bound_name(Loc::new(d.file, x)).as_deref() == Some(name.as_str())
            })
            .and_then(|(m, _)| self.c.sym_of_decl(Loc::new(d.file, m)))
    }

    /// `e` is `ns.x` or `ns[k]` with `ns` a namespace import.
    fn through_namespace_import(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(e.node) else {
            return false;
        };
        let obj = skip_parens(ast, obj);
        if !matches!(ast.node(obj), Node::Ident { .. }) {
            return false;
        }
        let Some(s) = self.c.resolve_ident(Loc::new(e.file, obj), meaning::VALUE) else {
            return false;
        };
        self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
            && self
                .c
                .first_decl(s)
                .is_some_and(|d| matches!(self.node(d), Node::ImportNamespaceSpecifier { .. }))
    }

    fn is_readonly_write_allowed(&self, e: Loc, p: SymId) -> bool {
        // Readonly properties may be assigned in the declaring class's constructor, through
        // `this` itself (not another instance, nor `(this)`).
        let Some(d) = self.c.value_decl(p) else {
            return false;
        };
        if !matches!(self.node(d), Node::PropertyDef { .. } | Node::Param { .. }) {
            return false;
        }
        let ast = self.ast(e.file);
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(e.node) else {
            return false;
        };
        if !matches!(ast.node(obj), Node::This {}) {
            return false;
        }
        let parents = self.c.file(e.file).parents();
        let Some(class_of_decl) = self.enclosing_class(d) else {
            return false;
        };
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::MethodDef {
                    kind: MethodKind::Constructor,
                    ..
                } => {
                    return self.enclosing_class(Loc::new(e.file, n)) == Some(class_of_decl);
                }
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::MethodDef { .. } => {
                    if let Some(pp) = parents.parent(n)
                        && matches!(
                            ast.node(pp),
                            Node::MethodDef {
                                kind: MethodKind::Constructor,
                                ..
                            }
                        )
                    {
                        cur = Some(pp);
                        continue;
                    }
                    return false;
                }
                Node::PropertyDef { .. } => {
                    return self.enclosing_class(Loc::new(e.file, n)) == Some(class_of_decl);
                }
                // Only an immediately invoked arrow runs as part of the constructor
                // (tsc 7: `(() => { this.a = 1; })()` is fine, `const f = () => ...` is not).
                Node::Arrow { .. } => {
                    let mut callee = n;
                    let mut up = parents.parent(n);
                    while let Some(u) = up
                        && matches!(ast.node(u), Node::Paren { .. })
                    {
                        callee = u;
                        up = parents.parent(u);
                    }
                    match up.map(|u| *ast.node(u)) {
                        Some(Node::Call { callee: c, .. }) if c == callee => {}
                        _ => return false,
                    }
                }
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// The class declaration enclosing a node.
    pub fn enclosing_class(&self, l: Loc) -> Option<Loc> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            if matches!(ast.node(n), Node::ClassDecl { .. } | Node::ClassExpr { .. }) {
                return Some(Loc::new(l.file, n));
            }
            cur = parents.parent(n);
        }
        None
    }

    /// TS2565 (tsc 7, strictNullChecks): `p` is declared by a property assignment
    /// (`X.p = v`, not `this.p`) in the same flow container as the read `e`, and no
    /// assignment through the same reference reaches `e` (`super.p`, or `X.p` read first).
    fn used_before_assignment_declaration(
        &mut self,
        e: Loc,
        obj: NodeId,
        p: SymId,
        t: TypeId,
    ) -> bool {
        if !self.c.opts.strict_null_checks {
            return false;
        }
        let ast = self.ast(e.file);
        if matches!(ast.node(skip_parens(ast, obj)), Node::This {}) {
            return false;
        }
        let Some(vd) = self.c.value_decl(p) else {
            return false;
        };
        if vd.file != e.file {
            return false;
        }
        let Node::Member { obj: dobj, .. } = *ast.node(vd.node) else {
            return false;
        };
        if matches!(ast.node(dobj), Node::This {}) {
            return false;
        }
        let is_assignment = self.c.parent(vd).is_some_and(|a| {
            matches!(*self.node(a), Node::Assign { left, op: ts_r_ast::AssignOp::Assign, .. } if left == vd.node)
        });
        if !is_assignment
            || !self.is_narrowable_reference(e)
            || self.flow_container(vd) != self.flow_container(e)
            || self.contains_undefined(t)
        {
            return false;
        }
        let optional = self.union(&[t, TypeId::UNDEFINED]);
        let ft = self.flow_type_of_reference(e, t, optional);
        self.contains_undefined(ft)
    }

    /// JavaScript `/** @private */ this.x = v` (or `@protected`): the accessibility the
    /// assignment statement's JSDoc gives the property it declares.
    fn jsdoc_assignment_access(&self, d: Loc) -> ts_r_ast::Modifiers {
        let ast = self.ast(d.file);
        if ast.jsdoc_tags.is_empty() || !self.c.file(d.file).is_js() {
            return ts_r_ast::Modifiers::NONE;
        }
        let parents = self.c.file(d.file).parents();
        let mut cur = Some(d.node);
        // The declaration is the `this.x` target; its statement is a few levels up.
        for _ in 0..4 {
            let Some(n) = cur else { break };
            for t in ast.jsdoc_tags_of(n) {
                match t.kind {
                    ts_r_ast::JsDocTagKind::Private => return ts_r_ast::Modifiers::PRIVATE,
                    ts_r_ast::JsDocTagKind::Protected => return ts_r_ast::Modifiers::PROTECTED,
                    _ => {}
                }
            }
            if matches!(ast.node(n), Node::ExprStmt { .. }) {
                break;
            }
            cur = parents.parent(n);
        }
        ts_r_ast::Modifiers::NONE
    }

    /// Whether a class enclosing `e` declares the private name `name` (instance or static).
    fn private_name_in_scope(&mut self, e: Loc, name: &str) -> bool {
        let mut cur = self.enclosing_class(e);
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 64 {
                break;
            }
            if let Some(cs) = self.c.sym_of_decl(c) {
                let inst = self.declared_type_of_symbol(cs);
                let stat = self.type_of_symbol(cs);
                if self.property_of_type(inst, name).is_some()
                    || self.property_of_type(stat, name).is_some()
                {
                    return true;
                }
            }
            cur = self.enclosing_class(c);
        }
        false
    }

    /// Whether class `outer` encloses class `inner`.
    fn class_encloses(&self, outer: Loc, inner: Loc) -> bool {
        let mut cur = self.enclosing_class(inner);
        for _ in 0..64 {
            match cur {
                Some(c) if c == outer => return true,
                Some(c) => cur = self.enclosing_class(c),
                None => return false,
            }
        }
        false
    }

    /// The member of class `class` declared with private name `name` (`#x`).
    fn private_member_named(&self, class: Loc, name: &str) -> Option<Loc> {
        let ast = self.ast(class.file);
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) =
            *ast.node(class.node)
        else {
            return None;
        };
        ast.list(members).iter().find_map(|&m| {
            let key = match *ast.node(m) {
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                _ => return None,
            };
            match *ast.node(key) {
                Node::PrivateName { name: n } if name.strip_prefix('#') == Some(ast.atom(n)) => {
                    Some(Loc::new(class.file, m))
                }
                _ => None,
            }
        })
    }

    fn check_property_accessibility(
        &mut self,
        pl: Loc,
        p: SymId,
        obj_type: TypeId,
        name: &str,
        is_super: bool,
    ) {
        if !self.reporting(pl.file) {
            return;
        }
        let _ = is_super;
        let decl = match self.c.synth_sym(p) {
            Some(x) if x.origins.len() == 1 => self
                .c
                .value_decl(x.origins[0])
                .or_else(|| self.c.first_decl(x.origins[0])),
            Some(_) => None,
            None => self.c.value_decl(p).or_else(|| self.c.first_decl(p)),
        };
        let Some(d) = decl else { return };
        let mods = self.node(d).modifiers() | self.jsdoc_assignment_access(d);
        // `super.m` where the nearest declaration of `m` is abstract (TS2513). Element
        // access (`super["m"]`) does not reach here, matching tsc.
        if is_super && mods.contains(ts_r_ast::Modifiers::ABSTRACT) {
            let class_name = self
                .enclosing_class(d)
                .and_then(|c| self.c.sym_of_decl(c))
                .map(|s| self.c.sym_name(s).to_owned());
            if let Some(class_name) = class_name {
                self.error(
                    pl,
                    dg::ABSTRACT_METHOD_0_IN_CLASS_1_CANNOT_BE_ACCESSED_VIA_SUPER_EXPRESSION_2513,
                    &[name, &class_name],
                );
            }
            return;
        }
        let private_name = name.starts_with('#');
        if !private_name
            && !mods.intersects(ts_r_ast::Modifiers::PRIVATE | ts_r_ast::Modifiers::PROTECTED)
        {
            return;
        }
        let Some(decl_class) = self.enclosing_class(d) else {
            return;
        };
        // Inside the declaring class (or, for protected, a subclass)?
        let mut cur = self.enclosing_class(pl);
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 64 {
                break;
            }
            if c == decl_class {
                return;
            }
            // A nested class declaring the same private name shadows it (TS18014).
            if private_name
                && let Some(shadow) = self.private_member_named(c, name)
                && self.class_encloses(decl_class, c)
            {
                let shown = self.type_to_string(obj_type);
                let mark = self.diags.len();
                self.error(
                    pl,
                    dg::THE_PROPERTY_0_CANNOT_BE_ACCESSED_ON_TYPE_1_WITHIN_THIS_CLASS_BECAUSE_IT_IS_SHAD_18014,
                    &[name, &shown],
                );
                self.relate_last(
                    mark,
                    shadow,
                    dg::THE_SHADOWING_DECLARATION_OF_0_IS_DEFINED_HERE_18017,
                    &[name],
                );
                self.relate_last(
                    mark,
                    d,
                    dg::THE_DECLARATION_OF_0_THAT_YOU_PROBABLY_INTENDED_TO_USE_IS_DEFINED_HERE_18018,
                    &[name],
                );
                return;
            }
            if mods.contains(ts_r_ast::Modifiers::PROTECTED)
                && self.class_derives_from(c, decl_class)
            {
                return;
            }
            cur = self.enclosing_class(c);
        }
        // `new Child().#foo` inside `Parent`, which declares its own `#foo`: the access
        // names `Parent`'s field, which a `Child` has by inheritance (observed: OK).
        if private_name {
            let mut cur = self.enclosing_class(pl);
            let mut guard = 0;
            while let Some(c) = cur {
                guard += 1;
                if guard > 64 {
                    break;
                }
                if self.class_derives_from(decl_class, c)
                    && let Some(cs) = self.c.sym_of_decl(c)
                {
                    let inst = self.declared_type_of_symbol(cs);
                    let own = self.property_of_type(inst, name).is_some_and(|q| {
                        self.c.value_decl(q).and_then(|qd| self.enclosing_class(qd)) == Some(c)
                    });
                    if own {
                        return;
                    }
                }
                cur = self.enclosing_class(c);
            }
        }
        let class_name = self
            .c
            .sym_of_decl(decl_class)
            .map(|s| self.c.sym_name(s).to_owned())
            .unwrap_or_default();
        if private_name {
            self.error(pl, dg::PROPERTY_0_IS_NOT_ACCESSIBLE_OUTSIDE_CLASS_1_BECAUSE_IT_HAS_A_PRIVATE_IDENTIFIER_18013, &[name, &class_name]);
        } else if mods.contains(ts_r_ast::Modifiers::PRIVATE) {
            self.error(
                pl,
                dg::PROPERTY_0_IS_PRIVATE_AND_ONLY_ACCESSIBLE_WITHIN_CLASS_1_2341,
                &[name, &class_name],
            );
        } else {
            self.error(pl, dg::PROPERTY_0_IS_PROTECTED_AND_ONLY_ACCESSIBLE_WITHIN_CLASS_1_AND_ITS_SUBCLASSES_2445, &[name, &class_name]);
        }
    }

    pub(crate) fn class_derives_from(&mut self, c: Loc, base: Loc) -> bool {
        let Some(cs) = self.c.sym_of_decl(c) else {
            return false;
        };
        let Some(bs) = self.c.sym_of_decl(base) else {
            return false;
        };
        // The base chain; a mixin base (`extends Mixin(Base)`) is an intersection whose
        // class members are all bases.
        let mut stack = vec![cs];
        let mut steps = 0;
        while let Some(cur) = stack.pop() {
            steps += 1;
            if steps > 64 {
                return false;
            }
            // Every base: an interface on the way (a class extending a global interface's
            // constructor) may extend several classes, one per merged declaration.
            let bases = self.base_types(cur);
            let mut parts = Vec::new();
            for &b in bases.iter() {
                if self.store().flags(b).contains(TypeFlags::INTERSECTION) {
                    parts.extend_from_slice(self.store().members_of_union(b));
                } else {
                    parts.push(b);
                }
            }
            for b in parts {
                let target = match self.store().object_of(b).map(|o| o.kind) {
                    Some(ts_r_types::ObjectKind::Reference { target, .. }) => target,
                    _ => b,
                };
                if let Some(ts_r_types::ObjectKind::Declared { sym, .. }) =
                    self.store().object_of(target).map(|o| o.kind)
                {
                    if sym == bs {
                        return true;
                    }
                    stack.push(sym);
                }
            }
        }
        false
    }

    /// TS2339 / TS2551 / TS2576 for a missing property.
    pub fn report_missing_property(&mut self, pl: Loc, name: &str, t: TypeId) {
        if !self.reporting(pl.file) {
            return;
        }
        // A missing member of `this` names the class type (`C1<T, U>`), not `this`.
        let shown = if self.store().type_param_data(t).is_some_and(|d| d.is_this) {
            self.type_param_constraint(t).unwrap_or(t)
        } else {
            t
        };
        let ts = self.type_to_string(shown);
        // Static member accessed through an instance.
        if let Some(cls) = self.class_symbol_of_instance(t)
            && let Some(stat) = self.type_of_symbol_opt(cls)
            && self.property_of_type(stat, name).is_some()
            && !name.starts_with('#')
        {
            let cname = self.c.sym_name(cls).to_owned();
            let full = format!("{cname}.{name}");
            self.error(
                pl,
                dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_DID_YOU_MEAN_TO_ACCESS_THE_STATIC_MEMBER_2_I_2576,
                &[name, &ts, &full],
            );
            return;
        }
        // A member of a later lib (`[].at` without es2022).
        if !name.starts_with('#')
            && let Some(lib) = self.target_library_hint(t, name)
        {
            self.error(
                pl,
                dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_DO_YOU_NEED_TO_CHANGE_YOUR_TARGET_LIBRARY_TR_2550,
                &[name, &ts, lib],
            );
            return;
        }
        let mut chain = Chain::new();
        let s = self.store();
        // An enum type (or a constraint naming one) and `boolean` are not elaborated member
        // by member.
        if s.flags(t).contains(TypeFlags::UNION)
            && !s.traits(t).contains(TypeTraits::ENUM_UNION)
            && t != TypeId::BOOLEAN
            && self.enum_of_literals(s.members_of_union(t)).is_none()
        {
            let members = s.members_of_union(t).to_vec();
            let nid = s.intern_str(name);
            for m in members {
                if self.property_of_type_id(m, nid).is_none()
                    && self.index_info_of(m, TypeId::STRING).is_none()
                {
                    let ms = self.type_to_string(m);
                    chain.push_back(dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339, &[name, &ms]);
                    break;
                }
            }
        }
        let suggestion = if name.starts_with('#') {
            None
        } else {
            self.spelling_suggestion_symbol_for_property(name, t)
        };
        match &suggestion {
            Some((sug, _)) => chain.push_front(
                dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_DID_YOU_MEAN_2_2551,
                &[name, &ts, sug],
            ),
            None => chain.push_front(dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339, &[name, &ts]),
        }
        let mark = self.diags.len();
        self.error_chain(pl, &chain);
        // "'<suggestion>' is declared here." at the suggested property's value declaration.
        if let Some((sug, sym)) = suggestion
            && let Some(d) = self.c.value_decl(sym)
        {
            self.relate_last(mark, d, dg::_0_IS_DECLARED_HERE_2728, &[&sug]);
        }
    }

    fn type_of_symbol_opt(&mut self, s: SymId) -> Option<TypeId> {
        Some(self.type_of_symbol(s))
    }

    /// The class symbol whose instance type `t` is.
    pub fn class_symbol_of_instance(&self, t: TypeId) -> Option<SymId> {
        let s = self.store();
        let o = s.object_of(t)?;
        let target = match o.kind {
            ts_r_types::ObjectKind::Reference { target, .. } => target,
            ts_r_types::ObjectKind::Declared { .. } => t,
            _ => return None,
        };
        match s.object_of(target)?.kind {
            ts_r_types::ObjectKind::Declared { sym, .. }
                if self.c.sym_flags(sym).intersects(SymbolFlags::CLASS) =>
            {
                Some(sym)
            }
            _ => None,
        }
    }

    /// Whether flow narrowing applies to a property access reference.
    pub fn is_narrowable_reference(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let mut n = e.node;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::Ident { .. }
                | Node::This {}
                | Node::Super {}
                | Node::MetaProperty {
                    kind: ts_r_ast::MetaPropKind::ImportMeta,
                } => return true,
                Node::Member { obj, .. } => n = obj,
                Node::Index { obj, index, .. } => {
                    if !matches!(
                        ast.node(skip_parens(ast, index)),
                        Node::StringLit { .. }
                            | Node::NumberLit { .. }
                            | Node::TemplateLit { .. }
                            | Node::Member { .. }
                            | Node::Ident { .. }
                    ) {
                        return false;
                    }
                    n = obj;
                }
                Node::Paren { expr } | Node::NonNull { expr } => n = expr,
                // `(f(), x).p` (an access through a comma expression only).
                Node::Seq { exprs } if n != e.node => match ast.list(exprs).last() {
                    Some(&l) => n = l,
                    None => return false,
                },
                _ => return false,
            }
        }
        false
    }

    /// `index` is the variable of a `for (let k in x)` statement and `obj` is the same
    /// identifier as `x` (tsc indexes with `number` when `x` has only a number index
    /// signature).
    fn is_for_in_key_access(&mut self, obj: Loc, index: Loc) -> bool {
        let ast = self.ast(index.file);
        let (Node::Ident { .. }, Node::Ident { .. }) = (
            ast.node(skip_parens(ast, index.node)),
            ast.node(skip_parens(ast, obj.node)),
        ) else {
            return false;
        };
        let Some(k) = self.c.resolve_ident(
            Loc::new(index.file, skip_parens(ast, index.node)),
            meaning::VALUE,
        ) else {
            return false;
        };
        let Some(o) = self.c.resolve_ident(
            Loc::new(obj.file, skip_parens(ast, obj.node)),
            meaning::VALUE,
        ) else {
            return false;
        };
        let Some(d) = self.c.first_decl(k) else {
            return false;
        };
        if d.file != index.file {
            return false;
        }
        // VarDeclarator -> VarDecl -> ForIn (as `left`).
        let mut cur = d;
        for _ in 0..3 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            if let Node::ForIn { left, right, .. } = *self.ast(p.file).node(p.node) {
                if left != cur.node {
                    return false;
                }
                let r = skip_parens(self.ast(p.file), right);
                if !matches!(self.ast(p.file).node(r), Node::Ident { .. }) {
                    return false;
                }
                return self.c.resolve_ident(Loc::new(p.file, r), meaning::VALUE) == Some(o);
            }
            cur = p;
        }
        false
    }

    /// A generic tuple read by a numeric literal in an expression: an element of its
    /// required prefix is that element; past it, with one variadic `...T` followed only by
    /// required elements, `T[number]` or one of those (`[...T][0]` is `T[number]`,
    /// `[...T, string][0]` is `string | T[number]`). The type-level `[...T][0]` stays
    /// deferred.
    fn variadic_tuple_element(&mut self, t: TypeId, index: TypeId) -> Option<TypeId> {
        let s = self.store();
        let shape = s.tuple_shape_of(t)?.clone();
        let v = s.number_value(index)?;
        if v < 0.0 || v.fract() != 0.0 {
            return None;
        }
        let i = v as usize;
        let args = self.c.type_args_of(t).to_vec();
        if args.len() < shape.elements.len() {
            return None;
        }
        let lead = shape
            .elements
            .iter()
            .take_while(|e| e.kind == ts_r_types::TupleElement::REQUIRED)
            .count();
        if i < lead {
            return Some(args[i]);
        }
        if lead >= shape.elements.len()
            || shape.elements[lead].kind != ts_r_types::TupleElement::VARIADIC
            || shape.elements[lead + 1..]
                .iter()
                .any(|e| e.kind != ts_r_types::TupleElement::REQUIRED)
        {
            return None;
        }
        let mut parts = vec![self.store().indexed_access(args[lead], TypeId::NUMBER)];
        parts.extend_from_slice(&args[lead + 1..shape.elements.len()]);
        Some(self.union(&parts))
    }

    /// `obj[index]`.
    /// The types of the properties of `t` named by each member of union key `key`, or
    /// `None` when some member is not a literal name of a property of `t`.
    fn union_key_properties(&mut self, t: TypeId, key: TypeId) -> Option<Vec<TypeId>> {
        let members = self.store().members_of_union(key).to_vec();
        let mut out = Vec::with_capacity(members.len());
        for m in members {
            let name = self.property_name_of_type(m)?;
            let nid = self.store().intern_str(&name);
            let p = self.property_of_type_id(t, nid)?;
            out.push(self.type_of_symbol(p));
        }
        Some(out)
    }

    pub fn element_access(
        &mut self,
        e: Loc,
        obj: NodeId,
        ot: TypeId,
        index: NodeId,
        is_write: bool,
    ) -> TypeId {
        let ot = self.reduced_type(ot);
        let f = e.file;
        let ol = Loc::new(f, obj);
        let il = Loc::new(f, index);
        let it = self.check_expr(il);
        if ot == TypeId::UNRESOLVED || it == TypeId::UNRESOLVED {
            return TypeId::UNRESOLVED;
        }
        if self.store().is_error(ot) {
            return TypeId::ERROR;
        }
        if self.store().flags(ot).intersects(TypeFlags::ANY) {
            return TypeId::ANY;
        }
        let nt = self.check_non_null(ol, ot);
        if self.store().is_error(nt) {
            return TypeId::ERROR;
        }
        // Any element of `never` is `never`, quietly (observed).
        if nt == TypeId::NEVER {
            return TypeId::NEVER;
        }
        // An access in an expression defers only for a generic key or a variadic tuple
        // object; a generic object with a concrete key reads its apparent type
        // (`t["a"]` with `T extends { a: string }` is `string`).
        if self.is_generic_index_type(it)
            || (self.is_generic_object_type(nt) && self.store().tuple_shape_of(nt).is_some())
        {
            if !self.is_generic_index_type(it)
                && let Some(r) = self.variadic_tuple_element(nt, it)
            {
                return r;
            }
            let r = self
                .indexed_access_type(nt, it, None)
                .unwrap_or(TypeId::UNRESOLVED);
            // Only for a conditional key, whose relation to `keyof` is modelled (through
            // its branches).
            if self.store().flags(r).contains(TypeFlags::INDEXED_ACCESS)
                && self.store().flags(it).contains(TypeFlags::CONDITIONAL)
                && !self.conditional_has_infer(it)
                && !self.is_valid_generic_index(nt, it)
            {
                let is = self.type_to_string(it);
                let os = self.type_to_string(nt);
                self.error(
                    e,
                    dg::TYPE_0_CANNOT_BE_USED_TO_INDEX_TYPE_1_2536,
                    &[&is, &os],
                );
            }
            // A constant generic key (`obj[key]` with `key: K`) in a constraint position
            // (the object of an access, a callee) narrows through the access's union
            // constraint (`if (obj[key]) obj[key].toUpperCase()` on `Record<K, string |
            // undefined>`).
            if !is_write
                && self.store().flags(r).contains(TypeFlags::INDEXED_ACCESS)
                && matches!(
                    self.ast(f).node(skip_parens(self.ast(f), index)),
                    Node::Ident { .. }
                )
                && self.is_narrowable_reference(e)
            {
                let d = self.narrowable_type_for_reference(e, r);
                if d != r {
                    return self.flow_type_of_reference(e, d, d);
                }
            }
            return r;
        }
        let apparent = self.apparent_type(nt);
        if apparent == TypeId::UNRESOLVED {
            return TypeId::UNRESOLVED;
        }
        // A `const` enum member is only read with a string literal key (TS2476), checked
        // by syntax (`E[k]` with `k: "A"` too).
        let string_key = {
            let ast = self.ast(f);
            match *ast.node(skip_parens(ast, index)) {
                Node::StringLit { .. } => true,
                Node::TemplateLit { parts } => ast.list(parts).len() == 1,
                _ => false,
            }
        };
        if !string_key && self.is_const_enum_object(ol) {
            if self.reporting(e.file) {
                self.error(
                    il,
                    dg::A_CONST_ENUM_MEMBER_CAN_ONLY_BE_ACCESSED_USING_A_STRING_LITERAL_2476,
                    &[],
                );
            }
            return TypeId::ERROR;
        }
        // A type parameter constrained to `any` (or an unresolved import's error type):
        // the access is `any`, as tsc gives it, so implicit-any checks still run downstream.
        if self.store().flags(apparent).intersects(TypeFlags::ANY) {
            return TypeId::ANY;
        }
        let itr = self.store().regular(it);
        // A union of literal keys that each name a property reads the union of those
        // properties (`tuple[i]` with `i: 0 | 1` has no `undefined` under
        // noUncheckedIndexedAccess).
        if !is_write
            && self.store().flags(itr).contains(TypeFlags::UNION)
            && let Some(parts) = self.union_key_properties(nt, itr)
        {
            let t = self.union(&parts);
            // A constant key narrows like a property (`if (o[k]) o[k].x`).
            if matches!(
                self.ast(f).node(skip_parens(self.ast(f), index)),
                Node::Ident { .. }
            ) && self.is_narrowable_reference(e)
            {
                return self.flow_type_of_reference(e, t, t);
            }
            return t;
        }
        // A literal key: a property.
        if let Some(name) = self.property_name_of_type(itr) {
            let nid = self.store().intern_str(&name);
            if let Some(p) = self.property_of_type_id(nt, nid) {
                if is_write && self.is_readonly_write(e, p) {
                    // A unique-symbol key shows as tsc names the property (`[g]`).
                    let shown = match self.c.symbol_key_display(&name) {
                        Some(d) => d,
                        None => name.clone(),
                    };
                    self.error(
                        il,
                        dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_READ_ONLY_PROPERTY_2540,
                        &[&shown],
                    );
                    return TypeId::ERROR;
                }
                let t = self.type_of_symbol(p);
                if is_write {
                    if let Some(w) = self.setter_type(p) {
                        return w;
                    }
                    if let Some(ps) = self.late_bound_setter(p)
                        && let Some(w) = self.setter_type(ps)
                    {
                        return w;
                    }
                    let wt = self.write_type_of_symbol(p);
                    return self.write_type_of_property(wt);
                }
                if self.is_narrowable_reference(e) {
                    return self.flow_type_of_reference(e, t, t);
                }
                return t;
            }
            // A missing member of a `const` enum is a missing property, at the key.
            if self.is_const_enum_object(ol) {
                let ts = self.type_to_string(nt);
                self.error(
                    il,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&name, &ts],
                );
                return TypeId::ERROR;
            }
            // Tuples: out of bounds.
            if let Some(shape) = self.store().tuple_shape_of(nt)
                && !shape.has_rest()
                && let Some(v) = self.store().number_value(itr)
                && v >= 0.0
                && v.fract() == 0.0
            {
                let len = shape.elements.len();
                let ts = self.type_to_string(nt);
                self.error(
                    il,
                    dg::TUPLE_TYPE_0_OF_LENGTH_1_HAS_NO_ELEMENT_AT_INDEX_2_2493,
                    &[&ts, &len.to_string(), &name],
                );
                return TypeId::UNDEFINED;
            }
            // A union of tuples none of which has the element: TS2339 naming the union.
            if let Some(v) = self.store().number_value(itr)
                && self.union_tuples_out_of_range(nt, v)
            {
                let ts = self.type_to_string(nt);
                self.error(
                    il,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&name, &ts],
                );
                return TypeId::UNDEFINED;
            }
            // Tuples with a rest element: an index past the leading elements reads the rest
            // element and every element after it (`[number, string, ...boolean[]][2]` is
            // `boolean`), under noUncheckedIndexedAccess with `undefined` from the count of
            // fixed elements on.
            if !is_write
                && let Some(v) = self.store().number_value(itr)
                && v >= 0.0
                && v.fract() == 0.0
                && let Some(r) = self.tuple_rest_region_element(nt, v as usize)
            {
                return r;
            }
        }
        // A branded key (`number & { __brand: never }`, `string & {}`) indexes as its
        // primitive.
        let itr = if self.some_member(itr, |m| {
            self.store().flags(m).contains(TypeFlags::INTERSECTION)
        }) {
            self.map_type(itr, |ck, m| {
                if !ck.store().flags(m).contains(TypeFlags::INTERSECTION) {
                    return m;
                }
                let parts = ck.store().members_of_union(m).to_vec();
                parts
                    .into_iter()
                    .find(|&p| {
                        ck.store().flags(p).intersects(
                            TypeFlags::STRING_LIKE
                                | TypeFlags::NUMBER_LIKE
                                | TypeFlags::ES_SYMBOL_LIKE,
                        )
                    })
                    .map_or(m, |p| ck.base_of_literal(p))
            })
        } else {
            itr
        };
        let f_it = self.store().flags(itr);
        // A `never` key reads `never` (`map[k as keyof {}]`).
        if f_it.contains(TypeFlags::NEVER) {
            return TypeId::NEVER;
        }
        // Valid key types.
        let key_ok = f_it.intersects(
            TypeFlags::STRING_LIKE
                | TypeFlags::NUMBER_LIKE
                | TypeFlags::ES_SYMBOL_LIKE
                | TypeFlags::ANY
                | TypeFlags::ENUM_LIKE,
        ) || self.every_member(itr, |m| {
            self.store().flags(m).intersects(
                TypeFlags::STRING_LIKE
                    | TypeFlags::NUMBER_LIKE
                    | TypeFlags::ES_SYMBOL_LIKE
                    | TypeFlags::ANY,
            )
        });
        if !key_ok {
            // Each failing member of a union key is reported (`boolean` whole, but as
            // `false` and `true` inside a larger union); a bigint literal written as the key is shown as `bigint`.
            let members =
                if itr != TypeId::BOOLEAN && self.store().flags(itr).contains(TypeFlags::UNION) {
                    self.store().members_of_union(itr).to_vec()
                } else {
                    vec![itr]
                };
            let ast = self.ast(il.file);
            let written_bigint =
                matches!(ast.node(skip_parens(ast, il.node)), Node::BigIntLit { .. });
            let mut reported = false;
            for m in members {
                let ok = self.store().flags(m).intersects(
                    TypeFlags::STRING_LIKE
                        | TypeFlags::NUMBER_LIKE
                        | TypeFlags::ES_SYMBOL_LIKE
                        | TypeFlags::ANY
                        | TypeFlags::ENUM_LIKE,
                );
                if ok {
                    continue;
                }
                let ks = if written_bigint {
                    "bigint".to_owned()
                } else {
                    self.type_to_string(m)
                };
                self.error(il, dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538, &[&ks]);
                reported = true;
            }
            if !reported {
                let ks = self.type_to_string(it);
                self.error(il, dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538, &[&ks]);
            }
            return TypeId::ERROR;
        }
        // `${number}` keys select number index signatures too, and so does the variable of
        // `for (const i in xs) xs[i]` when `xs` has only a number index signature (tsc).
        let numeric = self.every_member(itr, |m| {
            let s = self.store();
            s.flags(m).intersects(TypeFlags::NUMBER_LIKE)
                || s.template_parts(m).is_some_and(|(texts, types)| {
                    types.len() == 1
                        && types[0] == TypeId::NUMBER
                        && texts.iter().all(|&x| s.string_value(x) == Some(""))
                })
        }) || (f_it.intersects(TypeFlags::STRING)
            && self.index_info_of(nt, TypeId::STRING).is_none()
            && self.index_info_of(nt, TypeId::NUMBER).is_some()
            && self.is_for_in_key_access(ol, il));
        let symbolic = self.every_member(itr, |m| {
            self.store().flags(m).intersects(TypeFlags::ES_SYMBOL_LIKE)
        });
        // A string literal key spelling a number (`"0"`, `` `1` ``) selects the number index
        // signature first.
        let numeric_name = !numeric
            && self.store().string_value(itr).is_some_and(|k| {
                k.parse::<f64>()
                    .is_ok_and(|v| ts_r_binder::js_number_to_string(v) == k)
            });
        let info: Option<IndexInfo> = if f_it.intersects(TypeFlags::ANY) {
            // tsc 7 prefers the numeric index signature for an `any` key.
            self.index_info_of(nt, TypeId::NUMBER)
                .or_else(|| self.index_info_of(nt, TypeId::STRING))
        } else if numeric {
            self.index_info_of(nt, TypeId::NUMBER)
        } else if symbolic {
            self.index_info_of(nt, TypeId::SYMBOL)
        } else if numeric_name {
            self.index_info_of(nt, TypeId::NUMBER)
                .or_else(|| self.index_info_of(nt, TypeId::STRING))
        } else if self.is_pattern_literal_type(itr) {
            // A pattern key reads the index signatures it fits (`` `foo-${string}` ``).
            self.pattern_key_index_info(nt, itr)
                .or_else(|| self.index_info_of(nt, TypeId::STRING))
        } else {
            self.index_info_of(nt, TypeId::STRING)
        };
        if let Some(i) = info {
            if is_write && i.readonly {
                let ts = self.type_to_string(nt);
                self.error(
                    e,
                    dg::INDEX_SIGNATURE_IN_TYPE_0_ONLY_PERMITS_READING_2542,
                    &[&ts],
                );
                // The assigned value is still checked against the signature's type
                // (`a[0] = ""` on a `readonly number[]` is TS2542 and TS2322).
                return i.value;
            }
            let mut t = i.value;
            if self.c.opts.no_unchecked_indexed_access && !is_write {
                t = self.add_undefined(t);
            }
            // A literal key narrows like a property (`xs[0] ? xs[0].id : ''`).
            // So does a constant key (`o[key]` with a `const` or a parameter never assigned).
            if !is_write
                && (self.property_name_of_type(itr).is_some()
                    || matches!(
                        self.ast(f).node(skip_parens(self.ast(f), index)),
                        Node::Ident { .. }
                    ))
                && self.is_narrowable_reference(e)
            {
                return self.flow_type_of_reference(e, t, t);
            }
            return t;
        }
        if numeric && self.store().tuple_shape_of(nt).is_some() {
            return self
                .element_type_of_array_like(nt)
                .unwrap_or(TypeId::UNRESOLVED);
        }
        if f_it.intersects(TypeFlags::ANY) {
            // An `any` key (an unresolved import too) on a type without index signatures is
            // implicitly `any` (TS7053), shown as the apparent type (`{}` for `object`).
            if self.c.opts.no_implicit_any
                && self.reporting(e.file)
                && !self.is_for_in_key_access(ol, il)
            {
                let ts = if self.c.opts.strict_null_checks
                    && self.is_generic_object_type(nt)
                    && self.base_constraint(nt) == TypeId::UNKNOWN
                {
                    "unknown".to_owned()
                } else if self
                    .store()
                    .flags(apparent)
                    .intersects(TypeFlags::NON_PRIMITIVE)
                {
                    "{}".to_owned()
                } else {
                    self.type_to_string(apparent)
                };
                let ks = self.type_to_string(itr);
                self.error(
                    e,
                    dg::ELEMENT_IMPLICITLY_HAS_AN_ANY_TYPE_BECAUSE_EXPRESSION_OF_TYPE_0_CAN_T_BE_USED_TO_7053,
                    &[&ks, &ts],
                );
            }
            // The error type (shown as `any`): calling it takes type arguments quietly.
            return TypeId::ERROR;
        }
        // Union index with literal members: union of property types (of the apparent
        // properties for a generic object).
        if self.store().flags(itr).contains(TypeFlags::UNION) {
            if self.is_generic_object_type(nt) {
                let ms = self.store().members_of_union(itr).to_vec();
                let mut parts = Vec::with_capacity(ms.len());
                for m in ms {
                    let Some(name) = self.property_name_of_type(m) else {
                        break;
                    };
                    let nid = self.store().intern_str(&name);
                    let Some(p) = self.property_of_type_id(nt, nid) else {
                        break;
                    };
                    parts.push(self.type_of_symbol(p));
                }
                if parts.len() == self.store().members_of_union(itr).len() {
                    return self.union(&parts);
                }
            } else if is_write && let Some(w) = self.union_key_write_type(nt, itr) {
                return w;
            } else if let Some(t) = self.indexed_access_type(nt, itr, None) {
                // A constant key narrows like a property (`if (o[k]) o[k].x`).
                if !is_write
                    && matches!(
                        self.ast(f).node(skip_parens(self.ast(f), index)),
                        Node::Ident { .. }
                    )
                    && self.is_narrowable_reference(e)
                {
                    return self.flow_type_of_reference(e, t, t);
                }
                return t;
            }
        }
        // `for (const k in obj) obj[k]` on an `obj` declared with a generic type (tsc: an
        // indexed access with the key `Extract<keyof T, string>`; here `k` is a `string`, so
        // `obj` arrives as its constraint). On a concrete object the access falls through to
        // the implicit-any check (TS7053), as in tsc.
        if self.is_for_in_key_access(ol, il) {
            let ast = self.ast(f);
            let declared = self
                .c
                .resolve_ident(Loc::new(f, skip_parens(ast, obj)), meaning::VALUE)
                .map(|s| self.type_of_symbol(s));
            if declared.is_some_and(|t| self.is_generic_object_type(t)) {
                return TypeId::UNRESOLVED;
            }
        }
        // A fresh object literal (`{ a: 1, b: 'x' }[key]`): a `string` or `number` key reads
        // any property or `undefined`; a missing literal key is TS2339 and `undefined`.
        let const_asserted = {
            let ast = self.ast(ol.file);
            match *ast.node(skip_parens(ast, ol.node)) {
                Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => {
                    crate::nodes::is_keyword(ast, ty, KeywordTypeKind::Const)
                }
                _ => false,
            }
        };
        // Only for reads: an assignment to a missing key is implicitly `any` (TS7053).
        if !is_write
            && self.store().object_of(nt).is_some_and(|o| {
                o.flags.contains(ObjectFlags::OBJECT_LITERAL)
                    && (const_asserted || o.flags.contains(ObjectFlags::FRESH_LITERAL))
            })
        {
            let kf = self.store().flags(itr);
            // A union of literal keys reads the properties it names, and each missing key is
            // TS2339 (`{ a: 1 }[k]` with `k: "a" | "z"`).
            if kf.contains(TypeFlags::UNION) {
                let ms = self.store().members_of_union(itr).to_vec();
                let names: Vec<Option<String>> = ms
                    .iter()
                    .map(|&m| {
                        if self
                            .store()
                            .flags(m)
                            .intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
                        {
                            self.property_name_of_type(m)
                        } else {
                            None
                        }
                    })
                    .collect();
                if names.iter().all(Option::is_some) {
                    let mut parts: Vec<TypeId> = Vec::new();
                    let mut missing = false;
                    for name in names.into_iter().flatten() {
                        let nid = self.store().intern_str(&name);
                        if let Some(p) = self.property_of_type_id(nt, nid) {
                            parts.push(self.type_of_symbol(p));
                        } else {
                            missing = true;
                            if self.c.opts.no_implicit_any && self.reporting(e.file) {
                                let ts = self.type_to_string(nt);
                                self.error(
                                    e,
                                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                                    &[&name, &ts],
                                );
                            }
                        }
                    }
                    if missing {
                        parts.push(TypeId::UNDEFINED);
                    }
                    return self.union(&parts);
                }
            }
            if kf.intersects(TypeFlags::STRING | TypeFlags::NUMBER)
                && !kf.contains(TypeFlags::UNION)
            {
                let mut parts: Vec<TypeId> = Vec::new();
                for (_, p) in self.properties_of(nt) {
                    parts.push(self.type_of_symbol(p));
                }
                parts.push(TypeId::UNDEFINED);
                return self.union(&parts);
            }
            if kf.intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
                && let Some(name) = self.property_name_of_type(itr)
            {
                if self.c.opts.no_implicit_any && self.reporting(e.file) {
                    let ts = self.type_to_string(nt);
                    self.error(
                        e,
                        dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                        &[&name, &ts],
                    );
                }
                return TypeId::UNDEFINED;
            }
        }
        // A symbol key on an object with a string index signature but no symbol one cannot
        // index it (observed: TS2538, not an implicit `any`).
        if symbolic
            && !self.store().flags(itr).contains(TypeFlags::UNION)
            && self.store().flags(nt).contains(TypeFlags::OBJECT)
            && self.index_info_of(nt, TypeId::STRING).is_some()
            && self.reporting(e.file)
        {
            let ks = match self.store().kind(itr) {
                TypeKind::UniqueSymbol(_) => "unique symbol".to_owned(),
                _ => self.type_to_string(itr),
            };
            self.error(il, dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538, &[&ks]);
            return TypeId::ERROR;
        }
        if self.c.opts.no_implicit_any && self.reporting(e.file) {
            // A primitive is indexed as its apparent type (`Number`, and `String` with its
            // number index signature).
            let nt = if nt == TypeId::BOOLEAN {
                self.global_type(crate::checker::GlobalType::Boolean)
                    .unwrap_or(nt)
            } else if self.store().flags(nt).intersects(TypeFlags::PRIMITIVE)
                && !self.store().flags(nt).contains(TypeFlags::UNION)
                && self.store().flags(apparent).contains(TypeFlags::OBJECT)
            {
                apparent
            } else {
                nt
            };
            // An unconstrained type parameter is indexed as its apparent type `unknown`.
            let ts = if self.store().flags(nt).intersects(TypeFlags::INSTANTIABLE)
                && self.apparent_type(nt) == TypeId::UNKNOWN
            {
                "unknown".to_owned()
            } else if let Some(class) = self
                .store()
                .type_param_data(nt)
                .is_some_and(|d| d.is_this)
                .then(|| self.type_param_constraint(nt))
                .flatten()
            {
                // `this[k]` in a class shows the class (observed tsc 7).
                self.type_to_string(class)
            } else {
                self.type_to_string(nt)
            };
            // Arrays, tuples and objects with a `number` index signature report any other key
            // as not a number (TS7015), string literals included.
            let array_like = self.c.is_array_like_type(nt)
                || (self.store().flags(nt).contains(TypeFlags::UNION)
                    && self.every_member(nt, |m| self.c.is_array_like_type(m)));
            let number_indexed = array_like
                || (self.store().flags(nt).contains(TypeFlags::OBJECT)
                    && self
                        .resolved_members(nt)
                        .index
                        .iter()
                        .any(|i| i.key == TypeId::NUMBER))
                || (self.store().flags(nt).contains(TypeFlags::INTERSECTION)
                    && self.index_info_of(nt, TypeId::NUMBER).is_some());
            // A string literal key naming a static member, or spelled like a property, is
            // reported as the missing property (observed tsc 7).
            let literal_name = if self.store().flags(itr).contains(TypeFlags::STRING_LITERAL) {
                self.property_name_of_type(itr)
                    .filter(|n| !n.starts_with("__@") && !n.starts_with('#'))
            } else {
                None
            };
            let static_class = literal_name.as_ref().and_then(|name| {
                let cls = self.class_symbol_of_instance(nt)?;
                let stat = self.type_of_symbol(cls);
                self.property_of_type(stat, name).map(|_| cls)
            });
            let suggestion = match &literal_name {
                Some(name) if !number_indexed && static_class.is_none() => {
                    self.spelling_suggestion_symbol_for_property(name, nt)
                }
                _ => None,
            };
            // A global `let`/`const` is no property of the global object.
            let global_block = match &literal_name {
                Some(name) if !number_indexed => {
                    nt == self.global_this_type()
                        && self.c.global_symbol(name, meaning::VALUE).is_some_and(|g| {
                            self.c
                                .sym_flags(g)
                                .intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                        })
                }
                _ => false,
            };
            if !numeric && number_indexed {
                self.error(il, dg::ELEMENT_IMPLICITLY_HAS_AN_ANY_TYPE_BECAUSE_INDEX_EXPRESSION_IS_NOT_OF_TYPE_NUMBE_7015, &[]);
            } else if let Some(name) = literal_name.as_ref().filter(|_| global_block) {
                self.error(
                    e,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[name, &ts],
                );
            } else if let (Some(name), Some(cls)) = (&literal_name, static_class) {
                let full = format!("{}[{}]", self.c.sym_name(cls), self.c.text(il));
                self.error(
                    e,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_DID_YOU_MEAN_TO_ACCESS_THE_STATIC_MEMBER_2_I_2576,
                    &[name, &ts, &full],
                );
            } else if let (Some(name), Some((sug, sym))) = (&literal_name, suggestion) {
                let mark = self.diags.len();
                self.error(
                    il,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_DID_YOU_MEAN_2_2551,
                    &[name, &ts, &sug],
                );
                if let Some(d) = self.c.value_decl(sym) {
                    self.relate_last(mark, d, dg::_0_IS_DECLARED_HERE_2728, &[&sug]);
                }
            } else {
                // A `unique symbol` key is shown as such, its property as `[name]` (observed
                // tsc 7).
                let unique = match self.store().kind(itr) {
                    TypeKind::UniqueSymbol(sym) => Some(format!("[{}]", self.c.sym_name(sym))),
                    _ => None,
                };
                let ks = match unique {
                    Some(_) => "unique symbol".to_owned(),
                    None => self.type_to_string(itr),
                };
                let mut chain = Chain::new();
                chain.push_back(
                    dg::ELEMENT_IMPLICITLY_HAS_AN_ANY_TYPE_BECAUSE_EXPRESSION_OF_TYPE_0_CAN_T_BE_USED_TO_7053,
                    &[&ks, &ts],
                );
                if let Some(name) = unique {
                    chain.push_back(dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339, &[&name, &ts]);
                } else if let Some(name) = self.property_name_of_type(itr) {
                    if !name.starts_with("__@") {
                        chain
                            .push_back(dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339, &[&name, &ts]);
                    }
                } else if !self.store().flags(itr).intersects(
                    TypeFlags::TEMPLATE_LITERAL
                        | TypeFlags::STRING_MAPPING
                        | TypeFlags::ES_SYMBOL_LIKE,
                ) {
                    let base = self.base_of_literal(itr);
                    let bs = self.type_to_string(base);
                    chain.push_back(
                        dg::NO_INDEX_SIGNATURE_WITH_A_PARAMETER_OF_TYPE_0_WAS_FOUND_ON_TYPE_1_7054,
                        &[&bs, &ts],
                    );
                }
                self.error_chain(e, &chain);
            }
        }
        TypeId::ERROR
    }

    /// Whether a deferred access `obj[index]` in an expression is valid: the key is
    /// assignable to `keyof obj`, or a number-like key reads a generic object whose apparent
    /// type has a number index signature (an array-constrained `T`).
    fn is_valid_generic_index(&mut self, obj: TypeId, index: TypeId) -> bool {
        let k = self.keyof_type(obj);
        if self.is_assignable(index, k) {
            return true;
        }
        if self.is_generic_object_type(obj) && self.is_assignable(index, TypeId::NUMBER) {
            let a = self.apparent_type(obj);
            if a != TypeId::UNRESOLVED && self.index_info_of(a, TypeId::NUMBER).is_some() {
                return true;
            }
        }
        false
    }

    // ---------------------------------------------------------------------------------
    // Object and array literals
    // ---------------------------------------------------------------------------------

    /// tsc's `checkExpressionForMutableLocation`: literal types widen unless the position
    /// is a const context or the contextual type has literals of the same kind.
    pub fn check_expr_mutable(&mut self, e: Loc) -> TypeId {
        let t = self.check_expr(e);
        self.widen_for_mutable_location(e, t)
    }

    pub fn widen_for_mutable_location(&mut self, e: Loc, t: TypeId) -> TypeId {
        if !self.has_fresh_literal(t) {
            return t;
        }
        if self.in_const_context(e) {
            return self.regular_of_literal(t);
        }
        if let Some(ct) = self.contextual_type(e)
            && self.is_literal_of_contextual_type(t, ct)
        {
            return t;
        }
        self.widen_literal(t)
    }

    fn has_fresh_literal(&self, t: TypeId) -> bool {
        self.some_member(t, |m| {
            let s = self.store();
            s.traits(m).contains(TypeTraits::FRESH)
                && s.flags(m)
                    .intersects(TypeFlags::LITERAL | TypeFlags::ENUM_LITERAL)
        })
    }

    pub fn regular_of_literal(&mut self, t: TypeId) -> TypeId {
        if self.store().flags(t).contains(TypeFlags::UNION) {
            return self.map_type(t, |ck, m| ck.store().regular(m));
        }
        self.store().regular(t)
    }

    /// tsc's `isLiteralOfContextualType`.
    pub fn is_literal_of_contextual_type(&mut self, cand: TypeId, ct: TypeId) -> bool {
        let s = self.store();
        let cf = s.flags(ct);
        if cf.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(ct).to_vec();
            return ms
                .into_iter()
                .any(|m| self.is_literal_of_contextual_type(cand, m));
        }
        if cf.intersects(TypeFlags::INSTANTIABLE_NON_PRIMITIVE) {
            let c = self.base_constraint(ct);
            let c = if c == TypeId::UNRESOLVED {
                TypeId::UNKNOWN
            } else {
                c
            };
            let maybe = |ck: &Self, t: TypeId, f: TypeFlags| {
                ck.some_member(t, |m| ck.store().flags(m).intersects(f))
            };
            if (maybe(self, c, TypeFlags::STRING) && maybe(self, cand, TypeFlags::STRING_LITERAL))
                || (maybe(self, c, TypeFlags::NUMBER)
                    && maybe(self, cand, TypeFlags::NUMBER_LITERAL))
                || (maybe(self, c, TypeFlags::BIGINT)
                    && maybe(self, cand, TypeFlags::BIGINT_LITERAL))
                || (maybe(self, c, TypeFlags::ES_SYMBOL)
                    && maybe(self, cand, TypeFlags::UNIQUE_ES_SYMBOL))
            {
                return true;
            }
            if c == ct || c == TypeId::UNKNOWN {
                return false;
            }
            return self.is_literal_of_contextual_type(cand, c);
        }
        let maybe =
            |t: TypeId, f: TypeFlags| self.some_member(t, |m| self.store().flags(m).intersects(f));
        (cf.intersects(
            TypeFlags::STRING_LITERAL
                | TypeFlags::INDEX
                | TypeFlags::TEMPLATE_LITERAL
                | TypeFlags::STRING_MAPPING,
        ) && maybe(cand, TypeFlags::STRING_LITERAL))
            || (cf.intersects(TypeFlags::NUMBER_LITERAL) && maybe(cand, TypeFlags::NUMBER_LITERAL))
            || (cf.intersects(TypeFlags::BIGINT_LITERAL) && maybe(cand, TypeFlags::BIGINT_LITERAL))
            || (cf.intersects(TypeFlags::BOOLEAN_LITERAL)
                && maybe(cand, TypeFlags::BOOLEAN_LITERAL))
            || (cf.intersects(TypeFlags::UNIQUE_ES_SYMBOL)
                && maybe(cand, TypeFlags::UNIQUE_ES_SYMBOL))
    }

    /// `as const` contexts, and literals whose contextual type is a `const` type parameter
    /// (`f<const T>(x: T)` called as `f(['a'])`), at any level of the chain.
    pub fn in_const_context(&mut self, e: Loc) -> bool {
        self.is_const_context(e) || self.const_type_param_context(e).is_some()
    }

    /// Whether a literal (through parentheses, array elements, spreads and property values)
    /// has a `const` type parameter as its contextual type at some level. `Some(true)` when
    /// array literals become readonly tuples: not when the type parameter's constraint is
    /// a mutable array (`const T extends string[]`).
    pub fn const_type_param_context(&mut self, e: Loc) -> Option<bool> {
        if !self.c.has_const_type_params() {
            return None;
        }
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut n = e.node;
        for _ in 0..1000 {
            let literal = match *ast.node(n) {
                Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::TemplateLit { .. }
                | Node::ArrayLit { .. }
                | Node::ObjectLit { .. } => true,
                Node::Unary { op, arg } => {
                    matches!(op, UnaryOp::Minus)
                        && matches!(
                            ast.node(arg),
                            Node::NumberLit { .. } | Node::BigIntLit { .. }
                        )
                }
                _ => false,
            };
            if literal
                && let Some(ct) = self.contextual_type(Loc::new(e.file, n))
                && self.is_const_type_variable(ct, 0)
            {
                let c = self.base_constraint(ct);
                let members = match self.store().members_of_union(c) {
                    [] => vec![c],
                    m => m.to_vec(),
                };
                let mutable = members
                    .into_iter()
                    .any(|m| self.c.is_array_like_type(m) && !self.is_readonly_array_like(m));
                return Some(!mutable);
            }
            let p = parents.parent(n)?;
            match *ast.node(p) {
                Node::Paren { .. } | Node::ArrayLit { .. } | Node::Spread { .. } => n = p,
                Node::Property { value, .. } if value == n => n = parents.parent(p)?,
                Node::ShorthandProp { .. } => n = parents.parent(p)?,
                Node::Conditional { cons, alt, .. } if cons == n || alt == n => n = p,
                _ => return None,
            }
        }
        None
    }

    /// A `const` type parameter, or a union, intersection or indexed access over one.
    pub(crate) fn is_const_type_variable(&mut self, t: TypeId, depth: u32) -> bool {
        if depth > 5 {
            return false;
        }
        let st = self.store();
        let f = st.flags(t);
        if f.contains(TypeFlags::TYPE_PARAMETER) {
            let Some(d) = st.type_param_data(t) else {
                return false;
            };
            if d.is_this {
                return false;
            }
            let sym = d.sym;
            return self.c.decls(sym).into_iter().any(|l| {
                let ast = self.ast(l.file);
                matches!(*ast.node(l.node), Node::TypeParam { modifiers, .. }
                    if modifiers.contains(ts_r_ast::Modifiers::CONST))
            });
        }
        if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
            let members = st.members_of_union(t).to_vec();
            return members
                .into_iter()
                .any(|m| self.is_const_type_variable(m, depth));
        }
        match st.kind(t) {
            TypeKind::IndexedAccess { obj, .. } => self.is_const_type_variable(obj, depth + 1),
            TypeKind::Substitution { base, .. } => self.is_const_type_variable(base, depth + 1),
            TypeKind::Index(x) => self.is_const_type_variable(x, depth + 1),
            // A generic mapped type over a `const` type parameter's keys.
            _ if self.is_generic_mapped_type(t) => match self.mapped_info(t) {
                Some(info) => {
                    let c = self.mapped_constraint(&info);
                    self.is_const_type_variable(c, depth + 1)
                }
                None => false,
            },
            _ => false,
        }
    }

    /// `as const` contexts (through parentheses, array elements and property values).
    pub fn is_const_context(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut n = e.node;
        for _ in 0..1000 {
            let Some(p) = parents.parent(n) else {
                return false;
            };
            match *ast.node(p) {
                Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => {
                    return crate::nodes::is_keyword(ast, ty, KeywordTypeKind::Const);
                }
                Node::Paren { .. } | Node::ArrayLit { .. } | Node::Spread { .. } => n = p,
                Node::Property { value, .. } if value == n => {
                    n = parents.parent(p).unwrap_or(p);
                }
                Node::ShorthandProp { .. } => n = parents.parent(p).unwrap_or(p),
                Node::Conditional { cons, alt, .. } if cons == n || alt == n => n = p,
                _ => return false,
            }
        }
        false
    }

    fn check_array_literal(&mut self, e: Loc) -> TypeId {
        let ast = self.ast(e.file);
        let Node::ArrayLit { elements } = *ast.node(e.node) else {
            return TypeId::ERROR;
        };
        let const_assert = self.is_const_context(e);
        let const_param = if const_assert {
            None
        } else {
            self.const_type_param_context(e)
        };
        let const_ctx = const_assert || const_param.is_some();
        let ct = self.contextual_type(e);
        // A const assertion's tuple is mutable when its contextual type takes a mutable
        // array (`const a: T[] = [x] as const`, `[x] as const satisfies T[]`).
        let mutable_ctx = const_assert
            && ct.is_some_and(|c| {
                self.union_members(c)
                    .into_iter()
                    .any(|m| self.c.is_array_like_type(m) && !self.is_readonly_array_like(m))
            });
        let readonly_tuple = (const_assert && !mutable_ctx) || const_param == Some(true);
        let tuple_ctx = const_ctx || ct.is_some_and(|c| self.is_tuple_like_context(c));
        let mut els: Vec<TupleEl> = Vec::new();
        let mut elem_types: Vec<TypeId> = Vec::new();
        for &el in ast.list(elements) {
            let l = Loc::new(e.file, el);
            match *ast.node(el) {
                Node::Spread { arg } => {
                    let t = self.check_expr(Loc::new(e.file, arg));
                    if let Some(shape) = self.store().tuple_shape_of(t) {
                        let shape = shape.clone();
                        let args = self.c.type_args_of_tuple(t).to_vec();
                        for (i, x) in shape.elements.iter().enumerate() {
                            let a = args.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
                            // A rest element's type argument is its element type; the
                            // literal's rest element is the array of it.
                            let ty = if x.kind == ts_r_types::TupleElement::REST {
                                self.create_array(a, false)
                            } else {
                                a
                            };
                            els.push(TupleEl {
                                ty,
                                kind: x.kind,
                                label: None,
                            });
                            elem_types.push(
                                if x.kind == ts_r_types::TupleElement::REST
                                    || x.kind == ts_r_types::TupleElement::VARIADIC
                                {
                                    self.element_type_of_array_like(a).unwrap_or(a)
                                } else {
                                    a
                                },
                            );
                        }
                    } else {
                        let it = if self.store().flags(t).intersects(TypeFlags::ANY) {
                            t
                        } else {
                            self.spread_element_type(Loc::new(e.file, arg), t)
                        };
                        let arr = self.create_array(it, false);
                        els.push(TupleEl {
                            ty: arr,
                            kind: ts_r_types::TupleElement::REST,
                            label: None,
                        });
                        elem_types.push(it);
                    }
                }
                Node::Hole {} => {
                    let u = if self.c.opts.strict_null_checks {
                        TypeId::UNDEFINED
                    } else {
                        TypeId::UNDEFINED_WIDENING
                    };
                    els.push(TupleEl {
                        ty: u,
                        kind: ts_r_types::TupleElement::REQUIRED,
                        label: None,
                    });
                    elem_types.push(u);
                }
                _ => {
                    let t = self.check_expr_mutable(l);
                    els.push(TupleEl {
                        ty: t,
                        kind: ts_r_types::TupleElement::REQUIRED,
                        label: None,
                    });
                    elem_types.push(t);
                }
            }
        }
        if tuple_ctx {
            let t = self.create_tuple(&els, readonly_tuple);
            // Readonly tuples of array literals combine as inference candidates like object
            // literals do (`f4<const T>(x: [T, T])` with `[[1, 'x'], [2, 'y']]`).
            if const_ctx {
                self.const_array_literals.insert(t);
            }
            return t;
        }
        if elem_types.is_empty() {
            let el = if self.c.opts.strict_null_checks {
                TypeId::IMPLICIT_NEVER
            } else {
                TypeId::UNDEFINED_WIDENING
            };
            return self.create_array(el, false);
        }
        let u = self.union_with(&elem_types, Reduction::Subtype, AliasId::NONE);
        self.create_array(u, false)
    }

    pub(crate) fn is_tuple_like_context(&mut self, ct: TypeId) -> bool {
        let s = self.store();
        // A union with a tuple-like member, or an intersection with one (`number[] &
        // [number, ...number[]]` has a "0" property).
        if s.flags(ct).intersects(TypeFlags::UNION_OR_INTERSECTION) {
            let inter = s.flags(ct).contains(TypeFlags::INTERSECTION);
            let ms = s.members_of_union(ct).to_vec();
            // An array with a literal `length` (`number[] & { length: 2 }`).
            if inter
                && ms.iter().any(|&m| self.c.is_array_like_type(m))
                && let Some(p) = self.property_of_type(ct, "length")
            {
                let lt = self.type_of_symbol(p);
                if self.every_member(lt, |m| {
                    self.store().flags(m).contains(TypeFlags::NUMBER_LITERAL)
                }) {
                    return true;
                }
            }
            return ms.into_iter().any(|m| self.is_tuple_like_context(m));
        }
        if s.tuple_shape_of(ct).is_some() {
            return true;
        }
        if s.flags(ct).intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(ct);
            if c != ct && c != TypeId::UNKNOWN && c != TypeId::UNRESOLVED {
                return self.is_tuple_like_context(c);
            }
            return false;
        }
        if s.flags(ct).contains(TypeFlags::OBJECT) && self.is_homomorphic_generic_mapped(ct) {
            return true;
        }
        // Object types with a "0" property (but not arrays).
        if s.flags(ct).contains(TypeFlags::OBJECT)
            && !self.c.is_array_like_type(ct)
            && self.property_of_type(ct, "0").is_some()
        {
            return true;
        }
        // An array-like type with a literal `length` (`interface E extends [] {}`).
        if s.flags(ct).contains(TypeFlags::OBJECT)
            && !self.c.is_array_type(ct)
            && let Some(p) = self.property_of_type(ct, "length")
        {
            let lt = self.type_of_symbol(p);
            return self.every_member(lt, |m| {
                self.store().flags(m).contains(TypeFlags::NUMBER_LITERAL)
            });
        }
        false
    }

    /// A JavaScript object literal property with a JSDoc `@type {T}`: its value `t` must
    /// be assignable to `T` (reported at the key), and the property has type `T`.
    fn jsdoc_typed_property(&mut self, prop: Loc, key: NodeId, value: NodeId, t: TypeId) -> TypeId {
        let Some(declared) = self.jsdoc_property_type(prop) else {
            return t;
        };
        if t != TypeId::UNRESOLVED && !self.store().is_error(t) {
            let src = self.regular_of(t);
            self.check_assignable_with_elaboration(
                src,
                declared,
                Loc::new(prop.file, key),
                Some(Loc::new(prop.file, value)),
            );
        }
        declared
    }

    fn check_object_literal(&mut self, e: Loc) -> TypeId {
        let ast = self.ast(e.file);
        let Node::ObjectLit { props } = *ast.node(e.node) else {
            return TypeId::ERROR;
        };
        let const_ctx = self.in_const_context(e);
        let mut members: Vec<(StrId, SymId)> = Vec::new();
        // Values of members with non-literal computed keys, with their key kind.
        let mut index_types: Vec<(TypeId, IndexKeyKind)> = Vec::new();
        let mut contains_widening = false;
        let mut spread: Option<TypeId> = None;
        let mut seen: rustc_hash::FxHashMap<StrId, (usize, bool)> =
            rustc_hash::FxHashMap::default();
        for &m in ast.list(props) {
            let ml = Loc::new(e.file, m);
            let (key, ty, is_method, accessor) = match *ast.node(m) {
                Node::Property { key, value, .. } => {
                    let t = self.check_expr_mutable(Loc::new(e.file, value));
                    let t = self.jsdoc_typed_property(ml, key, value, t);
                    (Some(key), t, false, None)
                }
                Node::ShorthandProp { name, .. } => {
                    let t = self.check_expr_mutable(Loc::new(e.file, name));
                    let t = self.jsdoc_typed_property(ml, name, name, t);
                    (Some(name), t, false, None)
                }
                Node::MethodDef {
                    key, kind, value, ..
                } => match kind {
                    MethodKind::Get | MethodKind::Set => {
                        let t = self.check_function_expr(Loc::new(e.file, value));
                        let _ = t;
                        let sig = self.signature_from_decl(Loc::new(e.file, value));
                        let pt = if kind == MethodKind::Get {
                            // An unannotated getter has its setter's annotated type.
                            match self.object_literal_setter_annotation(e, key, value) {
                                Some(t) => t,
                                None => self.return_type_of(sig),
                            }
                        } else {
                            self.store()
                                .sig(sig)
                                .params
                                .first()
                                .map_or(TypeId::ANY, |p| p.ty)
                        };
                        (Some(key), pt, false, Some(kind))
                    }
                    _ => {
                        let t = self.check_function_expr(Loc::new(e.file, value));
                        (Some(key), t, true, None)
                    }
                },
                Node::Spread { arg } => {
                    let t = self.check_expr(Loc::new(e.file, arg));
                    // Flush the members so far (if any) into the spread.
                    if !members.is_empty() || !index_types.is_empty() || spread.is_none() {
                        let so_far = self.make_object_literal(
                            e,
                            std::mem::take(&mut members),
                            &mut index_types,
                            false,
                            contains_widening,
                        );
                        spread = Some(match spread {
                            Some(sp) => self.spread_types(sp, so_far),
                            None => so_far,
                        });
                    }
                    seen.clear();
                    let sp = spread.unwrap_or(TypeId::EMPTY_OBJECT);
                    spread = Some(self.spread_types(sp, t));
                    continue;
                }
                _ => continue,
            };
            let Some(key) = key else { continue };
            contains_widening |= self
                .store()
                .traits(ty)
                .contains(TypeTraits::CONTAINS_WIDENING);
            let name = match key_text(ast, key) {
                Some(n) => n,
                None => {
                    let Node::ComputedKey { expr } = *ast.node(key) else {
                        continue;
                    };
                    let kt = self.check_expr(Loc::new(e.file, expr));
                    match self.property_name_of_type(self.store().regular(kt)) {
                        Some(n) => n,
                        None => {
                            let _ = self.check_computed_key_type(Loc::new(e.file, key), kt);
                            let kind = self.index_key_kind(kt);
                            index_types.push((ty, kind));
                            continue;
                        }
                    }
                }
            };
            let nid = self.store().intern_str(&name);
            let mut flags = if is_method {
                SymbolFlags::METHOD
            } else {
                SymbolFlags::PROPERTY
            };
            let mut check = check_flags::OBJECT_LITERAL;
            if const_ctx
                || accessor == Some(MethodKind::Get) && !self.object_literal_has_setter(e, &name)
            {
                check |= check_flags::READONLY;
            }
            if accessor.is_some() {
                flags = SymbolFlags::PROPERTY;
            }
            if let Some(&(idx, was_accessor)) = seen.get(&nid) {
                if accessor.is_some() && was_accessor {
                    // getter/setter pair: keep the first (getter) entry.
                    continue;
                }
                if accessor.is_none() && !was_accessor && !is_method && self.reporting(e.file) {
                    let _ = idx;
                }
                members.retain(|(n, _)| *n != nid);
            }
            let s = self.c.new_synth(
                SynthSym::new(nid, flags, check)
                    .with_type(ty)
                    .with_decl(Some(ml)),
            );
            seen.insert(nid, (members.len(), accessor.is_some()));
            members.push((nid, s));
        }
        let trailing_empty = members.is_empty() && index_types.is_empty();
        let lit = self.make_object_literal(e, members, &mut index_types, true, contains_widening);
        match spread {
            Some(sp) => {
                if self.store().flags(sp).intersects(TypeFlags::ANY) {
                    return sp;
                }
                // Nothing after the last spread: a generic spread is not intersected with `{}`.
                if trailing_empty && self.is_generic_type(sp) {
                    return sp;
                }
                // Nor is a spread object, or a union of them (they keep their index
                // signatures: `{ ...(A | B) }`).
                if trailing_empty
                    && self.every_member(sp, |m| self.store().flags(m).contains(TypeFlags::OBJECT))
                {
                    return self.fresh_spread_literal(sp, e);
                }
                let r = self.spread_types(sp, lit);
                self.fresh_spread_literal(r, e)
            }
            None => lit,
        }
    }

    /// The fresh version of the object literal type `t` an object literal `e` with spreads
    /// produced: its own written properties are checked for excess, the spread ones not.
    fn fresh_spread_literal(&mut self, t: TypeId, e: Loc) -> TypeId {
        // (Only spreads: nothing of its own to check.)
        let ast = self.ast(e.file);
        if let Node::ObjectLit { props } = *ast.node(e.node)
            && ast
                .list(props)
                .iter()
                .all(|&p| matches!(ast.node(p), Node::Spread { .. }))
        {
            return t;
        }
        let s = self.store();
        let Some(o) = s.object_of(t) else { return t };
        let ts_r_types::ObjectKind::Literal { sym } = o.kind else {
            return t;
        };
        if !o.flags.contains(ObjectFlags::OBJECT_LITERAL)
            || o.flags.contains(ObjectFlags::FRESH_LITERAL)
        {
            return t;
        }
        let Some(m) = o.members() else { return t };
        let props: Vec<(StrId, SymId)> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .collect();
        let index = m.index.to_vec();
        let traits = s.traits(t);
        let f = s.literal_object(
            sym,
            o.flags | ObjectFlags::FRESH_LITERAL,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), index),
            traits,
        );
        s.set_regular(f, t);
        self.spread_literals.insert(f, e);
        f
    }

    /// The index kind a non-literal computed key type implies: `number` for number-like
    /// keys, `symbol` for symbol-like ones, else `string`.
    fn index_key_kind(&mut self, kt: TypeId) -> IndexKeyKind {
        if self.store().flags(kt).intersects(TypeFlags::ANY) {
            return IndexKeyKind::String;
        }
        if self.every_member(kt, |m| {
            self.store().flags(m).intersects(TypeFlags::NUMBER_LIKE)
        }) {
            IndexKeyKind::Number
        } else if self.every_member(kt, |m| {
            self.store().flags(m).intersects(TypeFlags::ES_SYMBOL_LIKE)
        }) {
            IndexKeyKind::Symbol
        } else {
            IndexKeyKind::String
        }
    }

    /// For the getter `getter` (its function) with key `key` in object literal `e` and no
    /// return annotation: the annotated parameter type of a setter of the same name.
    fn object_literal_setter_annotation(
        &mut self,
        e: Loc,
        key: NodeId,
        getter: NodeId,
    ) -> Option<TypeId> {
        let ast = self.ast(e.file);
        if crate::nodes::fn_parts(ast, getter)?.return_type.is_some() {
            return None;
        }
        let name = key_text(ast, key)?;
        let Node::ObjectLit { props } = *ast.node(e.node) else {
            return None;
        };
        let ann = ast.list(props).iter().find_map(|&m| match *ast.node(m) {
            Node::MethodDef {
                kind: MethodKind::Set,
                key: k,
                value,
                ..
            } if key_text(ast, k).as_deref() == Some(name.as_ref()) => {
                let parts = crate::nodes::fn_parts(ast, value)?;
                let p = ast
                    .list(parts.params)
                    .iter()
                    .copied()
                    .find(|&x| !crate::nodes::is_this_param(ast, x))?;
                match *ast.node(p) {
                    Node::Param { type_ann, .. } => type_ann,
                    _ => None,
                }
            }
            _ => None,
        })?;
        Some(self.type_from_node(Loc::new(e.file, ann)))
    }

    fn object_literal_has_setter(&self, e: Loc, name: &str) -> bool {
        let ast = self.ast(e.file);
        let Node::ObjectLit { props } = *ast.node(e.node) else {
            return false;
        };
        ast.list(props).iter().any(|&m| {
            matches!(*ast.node(m), Node::MethodDef { kind: MethodKind::Set, key, .. } if key_text(ast, key).as_deref() == Some(name))
        })
    }

    pub(crate) fn make_object_literal(
        &mut self,
        e: Loc,
        members: Vec<(StrId, SymId)>,
        index_types: &mut Vec<(TypeId, IndexKeyKind)>,
        fresh: bool,
        contains_widening: bool,
    ) -> TypeId {
        // A `string` index over the string-keyed members (number-keyed ones join it), a
        // `number` index over the number-keyed ones, a `symbol` index over the symbol-keyed
        // ones (`{ [n]: 1 }` with `n: number` is `{ [x: number]: number }`).
        let mut index = Vec::new();
        let has = |k: IndexKeyKind| index_types.iter().any(|&(_, x)| x == k);
        for (key, kinds) in [
            (
                TypeId::STRING,
                if has(IndexKeyKind::String) {
                    &[IndexKeyKind::String, IndexKeyKind::Number][..]
                } else {
                    &[][..]
                },
            ),
            (TypeId::NUMBER, &[IndexKeyKind::Number][..]),
            (TypeId::SYMBOL, &[IndexKeyKind::Symbol][..]),
        ] {
            let mut vals: Vec<TypeId> = index_types
                .iter()
                .filter(|(_, k)| kinds.contains(k))
                .map(|&(t, _)| t)
                .collect();
            if vals.is_empty() {
                continue;
            }
            // The named members join the index of their kind (all non-symbol ones the
            // `string` index, numeric names the `number` index).
            if key != TypeId::SYMBOL {
                for &(nid, p) in &members {
                    let name = self.store().str(nid).to_owned();
                    if name.starts_with("__@") || (key == TypeId::NUMBER && !is_numeric_name(&name))
                    {
                        continue;
                    }
                    vals.push(self.type_of_symbol(p));
                }
            }
            let v = self.union(&vals);
            index.push(IndexInfo {
                key,
                value: v,
                readonly: false,
                decl: None,
            });
        }
        index_types.clear();
        let sym = self.c.sym_of_decl(e).unwrap_or(SymId::NONE);
        let mut traits = TypeTraits::CONTAINS_OBJECT_LITERAL;
        let mut contains_tp = false;
        for &(_, p) in &members {
            if let Some(t) = self.c.synth_sym(p).and_then(|x| x.known_type()) {
                contains_tp |= self
                    .store()
                    .traits(t)
                    .contains(TypeTraits::CONTAINS_TYPE_PARAMETER);
            }
        }
        if contains_tp {
            traits |= TypeTraits::CONTAINS_TYPE_PARAMETER;
        }
        if contains_widening {
            traits |= TypeTraits::CONTAINS_WIDENING;
        }
        let mk =
            |ck: &mut Self, flags: ObjectFlags, members: &[(StrId, SymId)], index: &[IndexInfo]| {
                ck.store().literal_object(
                    sym,
                    flags,
                    ResolvedMembers::new(members.to_vec(), Vec::new(), Vec::new(), index.to_vec()),
                    traits,
                )
            };
        let regular = mk(self, ObjectFlags::OBJECT_LITERAL, &members, &index);
        if !fresh {
            return regular;
        }
        let f = mk(
            self,
            ObjectFlags::OBJECT_LITERAL | ObjectFlags::FRESH_LITERAL,
            &members,
            &index,
        );
        self.store().set_regular(f, regular);
        f
    }

    /// `{ ...left, ...right }`.
    /// A spread result that takes part in a union: not normalized with its siblings when
    /// widened (`{ ...(X | Y) }` stays `X' | Y'`).
    fn without_literal_trait(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        let Some(o) = s.object_of(t) else { return t };
        if !matches!(o.kind, ts_r_types::ObjectKind::Literal { .. }) {
            return t;
        }
        let Some(m) = o.members() else { return t };
        let props: Vec<(StrId, SymId)> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .collect();
        let rm = ResolvedMembers::new(
            props,
            m.call.to_vec(),
            m.construct.to_vec(),
            m.index.to_vec(),
        );
        let flags = o.flags;
        let sym = match o.kind {
            ts_r_types::ObjectKind::Literal { sym } => sym,
            _ => SymId::NONE,
        };
        s.literal_object(sym, flags, rm, TypeTraits::NONE)
    }

    /// `x`'s properties, all optional (`...(c ? x : {})` spreads this).
    fn optional_spread_of(&mut self, x: TypeId) -> TypeId {
        let mut props = Vec::new();
        for (n, p) in self.properties_of(x) {
            if self.c.sym_flags(p).intersects(SymbolFlags::METHOD) && self.is_class_instance_type(x)
            {
                continue;
            }
            let pt = self.type_of_symbol(p);
            let ty = self.union2(pt, TypeId::UNDEFINED);
            let decl = self.c.synth_sym(p).and_then(|x| x.decl);
            let np = self.c.new_synth(
                SynthSym::new(
                    n,
                    SymbolFlags::PROPERTY | SymbolFlags::OPTIONAL,
                    check_flags::OBJECT_LITERAL,
                )
                .with_type(ty)
                .with_decl(decl),
            );
            props.push((n, np));
        }
        self.store().literal_object(
            SymId::NONE,
            ObjectFlags::OBJECT_LITERAL,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), Vec::new()),
            TypeTraits::CONTAINS_OBJECT_LITERAL,
        )
    }

    pub(crate) fn spread_types(&mut self, left: TypeId, right: TypeId) -> TypeId {
        let s = self.store();
        let rf = s.flags(right);
        if rf.intersects(TypeFlags::ANY) || s.flags(left).intersects(TypeFlags::ANY) {
            return if right == TypeId::UNRESOLVED || left == TypeId::UNRESOLVED {
                TypeId::UNRESOLVED
            } else {
                TypeId::ANY
            };
        }
        // A generic spread stays generic: `{ ...t }` is `T`, `{ a, ...t }` is `{ a } & T`.
        // Nullish members of a generic union spread nothing (`T | undefined` spreads `T`).
        let original = right;
        let right = if rf.contains(TypeFlags::UNION) {
            self.non_nullable_strict(right)
        } else {
            right
        };
        if self.is_generic_type(right) || self.is_generic_type(left) {
            if self.is_empty_object_type(left) {
                return right;
            }
            return self.intersection(&[left, right]);
        }
        // Unions of object types: `X | {}` and `X | undefined` spread `X`'s properties as
        // optional; other unions distribute (`{ a, ...(X | Y) }` is
        // `{ a, ...X } | { a, ...Y }`, a nullish member giving `{ a }`).
        if rf.contains(TypeFlags::UNION) {
            let ms = self.store().members_of_union(original).to_vec();
            let nullish = |ck: &Self, m: TypeId| {
                ck.store()
                    .flags(m)
                    .intersects(TypeFlags::NULLABLE | TypeFlags::VOID)
            };
            if ms.iter().all(|&m| {
                nullish(self, m)
                    || self
                        .store()
                        .flags(m)
                        .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
            }) {
                if ms.len() == 2 {
                    let e0 = nullish(self, ms[0]) || self.is_empty_object_type(ms[0]);
                    let e1 = nullish(self, ms[1]) || self.is_empty_object_type(ms[1]);
                    if e0 != e1 {
                        let x = if e0 { ms[1] } else { ms[0] };
                        let partial = self.optional_spread_of(x);
                        return self.spread_types(left, partial);
                    }
                }
                if ms.len() <= 32 {
                    let mut parts: Vec<TypeId> = Vec::with_capacity(ms.len());
                    for &m in &ms {
                        let p = if nullish(self, m) {
                            left
                        } else {
                            self.spread_types(left, m)
                        };
                        parts.push(self.without_literal_trait(p));
                    }
                    return self.union(&parts);
                }
            }
        }
        let right = self.non_nullable_strict(right);
        let rf = self.store().flags(right);
        if rf.contains(TypeFlags::NEVER) || rf.intersects(TypeFlags::NULLABLE | TypeFlags::VOID) {
            return left;
        }
        if self.store().flags(left).contains(TypeFlags::UNION)
            && rf.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            let ms = self.store().members_of_union(left).to_vec();
            if ms.len() <= 32
                && ms.iter().all(|&m| {
                    self.store()
                        .flags(m)
                        .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                })
            {
                let mut parts: Vec<TypeId> = Vec::with_capacity(ms.len());
                for &m in &ms {
                    let p = self.spread_types(m, right);
                    parts.push(self.without_literal_trait(p));
                }
                return self.union(&parts);
            }
        }
        if rf.intersects(TypeFlags::UNION | TypeFlags::INSTANTIABLE)
            || self
                .store()
                .flags(left)
                .intersects(TypeFlags::UNION | TypeFlags::INSTANTIABLE)
        {
            if self.is_generic_type(right) || self.is_generic_type(left) {
                return self.intersection(&[left, right]);
            }
            return TypeId::UNRESOLVED;
        }
        if !rf.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION) {
            return left;
        }
        let mut props: Vec<(StrId, SymId)> = if left == TypeId::EMPTY_OBJECT {
            Vec::new()
        } else {
            self.properties_of(left)
        };
        let rprops = self.properties_of(right);
        for (n, p) in rprops {
            // Methods of class instances and `#private` fields are not spread.
            if (self.c.sym_flags(p).intersects(SymbolFlags::METHOD)
                && self.is_class_instance_type(right))
                || self.store().str(n).starts_with('#')
            {
                continue;
            }
            let pt = self.type_of_symbol(p);
            let optional = self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL);
            let ty = if optional {
                match props.iter().find(|(m, _)| *m == n) {
                    Some(&(_, lp)) => {
                        let lt = self.type_of_symbol(lp);
                        let pt2 = self.remove_missing_undefined(pt);
                        self.union2(lt, pt2)
                    }
                    None => pt,
                }
            } else {
                pt
            };
            // Optional when the left side lacks it or has it optional too.
            let mut flags = SymbolFlags::PROPERTY;
            if optional
                && props
                    .iter()
                    .find(|(m, _)| *m == n)
                    .is_none_or(|&(_, lp)| self.c.sym_flags(lp).intersects(SymbolFlags::OPTIONAL))
            {
                flags |= SymbolFlags::OPTIONAL;
            }
            let decl = self.c.synth_sym(p).and_then(|x| x.decl);
            // A copy of a declared property keeps its declarations (as tsc's spread does):
            // declaration emit writes it from its annotation.
            let origins = if decl.is_none() {
                // Keep the declaration sources, without chaining through every spread.
                match self.c.synth_sym(p) {
                    Some(x) => x.origins.to_vec(),
                    None => vec![p],
                }
            } else {
                Vec::new()
            };
            let np = self.c.new_synth(
                SynthSym::new(n, flags, check_flags::OBJECT_LITERAL)
                    .with_type(ty)
                    .with_decl(decl)
                    .with_origins(origins),
            );
            props.retain(|(m, _)| *m != n);
            props.push((n, np));
        }
        // Index signatures: the right side's when nothing precedes it, otherwise those both
        // sides have (`{ ...h }` keeps `h`'s, `{ ...h, a }` and `{ a, ...h }` lose them).
        // (A spread's signatures are writable: `{ ...ro }` of `{ readonly [k: string]: T }`
        // takes writes.)
        let mut index = self.spread_index_infos(left, right);
        for i in &mut index {
            i.readonly = false;
        }
        let sym = SymId::NONE;
        self.store().literal_object(
            sym,
            ObjectFlags::OBJECT_LITERAL,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), index),
            TypeTraits::CONTAINS_OBJECT_LITERAL,
        )
    }

    fn spread_index_infos(&mut self, left: TypeId, right: TypeId) -> Vec<IndexInfo> {
        let infos = |ck: &mut Self, t: TypeId| -> Vec<IndexInfo> {
            let f = ck.store().flags(t);
            if f.contains(TypeFlags::OBJECT) {
                ck.resolved_members(t).index.to_vec()
            } else if f.contains(TypeFlags::INTERSECTION) {
                // An intersection's signatures for each key its members declare, their
                // value types intersected (`{ ...(Headers & { [k: string]: string }) }`).
                let mut out: Vec<IndexInfo> = Vec::new();
                let mut values: Vec<Vec<TypeId>> = Vec::new();
                for m in ck.store().members_of_union(t).to_vec() {
                    if !ck.store().flags(m).contains(TypeFlags::OBJECT) {
                        continue;
                    }
                    for &i in ck.resolved_members(m).index.iter() {
                        match out.iter().position(|o| o.key == i.key) {
                            Some(k) => {
                                values[k].push(i.value);
                                out[k].readonly &= i.readonly;
                                // (Printed with the declared parameter name only when one
                                // member declares it.)
                                out[k].decl = None;
                            }
                            None => {
                                out.push(i);
                                values.push(vec![i.value]);
                            }
                        }
                    }
                }
                for (o, v) in out.iter_mut().zip(values) {
                    o.value = ck.intersection(&v);
                }
                out
            } else {
                Vec::new()
            }
        };
        let r = infos(self, right);
        if r.is_empty() {
            return r;
        }
        let empty_left = left == TypeId::EMPTY_OBJECT
            || (self.store().flags(left).contains(TypeFlags::OBJECT)
                && self
                    .object_flags(left)
                    .contains(ObjectFlags::OBJECT_LITERAL)
                && {
                    let m = self.resolved_members(left);
                    m.props.is_empty() && m.index.is_empty()
                });
        if empty_left {
            return r;
        }
        let l = infos(self, left);
        let mut out = Vec::new();
        for li in l {
            if let Some(ri) = r.iter().find(|ri| ri.key == li.key) {
                let value = self.union2(li.value, ri.value);
                out.push(IndexInfo {
                    key: li.key,
                    value,
                    readonly: li.readonly || ri.readonly,
                    decl: None,
                });
            }
        }
        out
    }

    fn remove_missing_undefined(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        self.filter_type(t, |ck, m| {
            !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
        })
    }

    /// Widening of an object literal type for a declaration: nested object literals and
    /// `null`/`undefined` widen; the result is not fresh.
    pub fn widen_object_literal(&mut self, t: TypeId) -> TypeId {
        self.widen_object_literal_in(t, None)
    }

    /// [`Self::widen_object_literal`] for a member of a union of object literals:
    /// `siblings` are the union's members (see `widened_type_in`).
    pub(crate) fn widen_object_literal_in(
        &mut self,
        t: TypeId,
        siblings: Option<&[TypeId]>,
    ) -> TypeId {
        if siblings.is_none()
            && let Some(&w) = self.widened.get(&t)
        {
            return w;
        }
        let s = self.store();
        let Some(o) = s.object_of(t) else { return t };
        let flags = o.flags.without(ObjectFlags::FRESH_LITERAL);
        let sym = match o.kind {
            ts_r_types::ObjectKind::Literal { sym } => sym,
            _ => return t,
        };
        let Some(m) = o.members() else { return t };
        let props: Vec<(StrId, SymId)> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .collect();
        let index = m.index.to_vec();
        let call = m.call.to_vec();
        let mut out = Vec::with_capacity(props.len());
        let mut changed =
            s.traits(t).contains(TypeTraits::FRESH) || o.flags.contains(ObjectFlags::FRESH_LITERAL);
        let own: Vec<StrId> = props.iter().map(|&(n, _)| n).collect();
        for (n, p) in props {
            let pt = self.type_of_symbol(p);
            // Nested object literals widen with the same property of each sibling.
            let child = match siblings {
                Some(sibs)
                    if self
                        .store()
                        .traits(pt)
                        .contains(TypeTraits::CONTAINS_OBJECT_LITERAL)
                        || self.is_object_literal_type(pt) =>
                {
                    let mut c = Vec::new();
                    for &sib in sibs {
                        if !self.is_object_literal_type(sib) {
                            continue;
                        }
                        let sp = self
                            .store()
                            .object_of(sib)
                            .and_then(|o| o.members())
                            .and_then(|m| m.find(n));
                        if let Some(sp) = sp {
                            let st = self.type_of_symbol(sp);
                            c.extend(self.union_members(st));
                        }
                    }
                    Some(c)
                }
                _ => None,
            };
            let wt = self.widened_type_in(pt, child.as_deref());
            if wt != pt {
                changed = true;
                let f = self.c.sym_flags(p);
                let ch = self.c.sym_check_flags(p);
                let decl = self.c.synth_sym(p).and_then(|x| x.decl);
                let np = self.c.new_synth(
                    SynthSym::new(n, f, ch)
                        .with_type(wt)
                        .with_decl(decl)
                        .with_origins(vec![p]),
                );
                out.push((n, np));
            } else {
                out.push((n, p));
            }
        }
        // Properties only siblings have: optional, of type `undefined`.
        if let Some(sibs) = siblings {
            let mut seen = own;
            for &sib in sibs {
                if sib == t || !self.is_object_literal_type(sib) {
                    continue;
                }
                let Some(sm) = self.store().object_of(sib).and_then(|o| o.members()) else {
                    continue;
                };
                let entries: Vec<(StrId, SymId)> = sm
                    .names
                    .iter()
                    .copied()
                    .zip(sm.props.iter().copied())
                    .collect();
                for (n, p) in entries {
                    if seen.contains(&n) {
                        continue;
                    }
                    seen.push(n);
                    let decl = self.c.synth_sym(p).and_then(|x| x.decl);
                    let np = self.c.new_synth(
                        SynthSym::new(n, SymbolFlags::PROPERTY | SymbolFlags::OPTIONAL, 0)
                            .with_type(TypeId::UNDEFINED)
                            .with_decl(decl),
                    );
                    out.push((n, np));
                    changed = true;
                }
            }
        }
        let s = self.store();
        let r = if changed {
            let traits = s
                .traits(t)
                .without(TypeTraits::FRESH | TypeTraits::CONTAINS_WIDENING);
            s.literal_object(
                sym,
                flags,
                ResolvedMembers::new(out, call, Vec::new(), index),
                traits,
            )
        } else {
            t
        };
        if siblings.is_none() {
            self.widened.insert(t, r);
        }
        r
    }

    /// An object literal expression's type (fresh or widened).
    fn is_object_literal_type(&self, t: TypeId) -> bool {
        self.store()
            .object_of(t)
            .is_some_and(|o| o.flags.contains(ObjectFlags::OBJECT_LITERAL))
    }

    // ---------------------------------------------------------------------------------
    // Operators
    // ---------------------------------------------------------------------------------

    /// Truthiness facts of a type.
    pub fn truthiness(&mut self, t: TypeId) -> Truthiness {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            let mut r = Truthiness {
                truthy: false,
                falsy: false,
            };
            for m in ms {
                let x = self.truthiness(m);
                r.truthy |= x.truthy;
                r.falsy |= x.falsy;
            }
            return r;
        }
        let both = Truthiness {
            truthy: true,
            falsy: true,
        };
        if f.intersects(TypeFlags::ANY | TypeFlags::UNKNOWN) {
            return both;
        }
        if f.contains(TypeFlags::NEVER) {
            return Truthiness {
                truthy: false,
                falsy: false,
            };
        }
        if f.intersects(TypeFlags::NULLABLE | TypeFlags::VOID) {
            return Truthiness {
                truthy: false,
                falsy: true,
            };
        }
        if f.intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(t);
            if c == t || c == TypeId::UNKNOWN || c == TypeId::UNRESOLVED {
                return both;
            }
            return self.truthiness(c);
        }
        if f.intersects(TypeFlags::LITERAL) || f.contains(TypeFlags::ENUM_LITERAL) {
            let falsy = match s.kind(t) {
                TypeKind::Intrinsic => t == TypeId::FALSE || t == TypeId::FRESH_FALSE,
                _ => {
                    if let Some(v) = s.string_value(t) {
                        v.is_empty()
                    } else if let Some(v) = s.number_value(t) {
                        v == 0.0 || v.is_nan()
                    } else if let TypeKind::BigIntLiteral(x) = s.kind(t) {
                        s.str(x) == "0"
                    } else {
                        false
                    }
                }
            };
            return Truthiness {
                truthy: !falsy,
                falsy,
            };
        }
        if f.intersects(
            TypeFlags::STRING
                | TypeFlags::NUMBER
                | TypeFlags::BIGINT
                | TypeFlags::BOOLEAN
                | TypeFlags::ENUM,
        ) {
            return both;
        }
        // Objects, symbols, functions.
        Truthiness {
            truthy: true,
            falsy: false,
        }
    }

    /// Members of `t` that can be truthy (`boolean` → `true`).
    pub fn remove_definitely_falsy(&mut self, t: TypeId) -> TypeId {
        self.map_type(t, |ck, m| {
            if m == TypeId::BOOLEAN {
                return TypeId::TRUE;
            }
            if ck.truthiness(m).truthy {
                m
            } else {
                TypeId::NEVER
            }
        })
    }

    /// The definitely-falsy parts of `t` (`string` → `""`, `number` → `0`, ...).
    pub fn extract_definitely_falsy(&mut self, t: TypeId) -> TypeId {
        self.map_type(t, |ck, m| {
            let s = ck.store();
            let f = s.flags(m);
            if f.contains(TypeFlags::STRING) {
                return s.string_literal("", false);
            }
            if f.contains(TypeFlags::NUMBER) {
                return s.number_literal(0.0, false);
            }
            if f.contains(TypeFlags::BIGINT) {
                return s.bigint_literal("0", false);
            }
            if m == TypeId::BOOLEAN || f.contains(TypeFlags::BOOLEAN) {
                return TypeId::FALSE;
            }
            // `any` and `unknown` may be falsy values of any kind: kept whole.
            if f.intersects(TypeFlags::ANY | TypeFlags::UNKNOWN) {
                return m;
            }
            let tr = ck.truthiness(m);
            if tr.falsy && !tr.truthy {
                m
            } else {
                TypeId::NEVER
            }
        })
    }

    /// Checks a condition expression (`if`, `while`, `?:`, `!`, `&&`/`||` operands).
    pub fn check_truthiness_expr(&mut self, e: Loc) -> TypeId {
        self.check_condition_shape(e);
        let t = self.check_expr(e);
        if self.store().flags(t).contains(TypeFlags::VOID) {
            self.error(
                e,
                dg::AN_EXPRESSION_OF_TYPE_VOID_CANNOT_BE_TESTED_FOR_TRUTHINESS_1345,
                &[],
            );
        }
        t
    }

    fn check_unary(&mut self, e: Loc, op: UnaryOp, arg: NodeId) -> TypeId {
        let al = Loc::new(e.file, arg);
        match op {
            UnaryOp::Not => {
                let t = self.check_truthiness_expr(al);
                let tr = self.truthiness(t);
                match (tr.truthy, tr.falsy) {
                    (true, false) => TypeId::FALSE,
                    (false, true) => TypeId::TRUE,
                    _ => TypeId::BOOLEAN,
                }
            }
            UnaryOp::Minus | UnaryOp::Plus => {
                let ast = self.ast(e.file);
                if let Node::NumberLit { value } = *ast.node(arg) {
                    let v = if op == UnaryOp::Minus {
                        -value.get()
                    } else {
                        value.get()
                    };
                    self.check_expr(al);
                    return self.store().number_literal(v, true);
                }
                if let (UnaryOp::Minus, Node::BigIntLit { value }) = (op, *ast.node(arg)) {
                    self.check_expr(al);
                    let text = bigint_text(ast.atom(value));
                    let neg = if text == "0" {
                        text
                    } else {
                        format!("-{text}")
                    };
                    return self.store().bigint_literal(&neg, true);
                }
                let t = self.check_expr(al);
                let t = self.check_non_null(al, t);
                if self.store().flags(t).intersects(TypeFlags::ES_SYMBOL_LIKE) {
                    let o = if op == UnaryOp::Minus { "-" } else { "+" };
                    self.error(
                        al,
                        dg::THE_0_OPERATOR_CANNOT_BE_APPLIED_TO_TYPE_SYMBOL_2469,
                        &[o],
                    );
                }
                if op == UnaryOp::Minus
                    && self.every_member(t, |m| {
                        self.store().flags(m).intersects(TypeFlags::BIGINT_LIKE)
                    })
                    && t != TypeId::NEVER
                {
                    return TypeId::BIGINT;
                }
                TypeId::NUMBER
            }
            UnaryOp::BitNot => {
                let t = self.check_expr(al);
                let t = self.check_non_null(al, t);
                if self.every_member(t, |m| {
                    self.store().flags(m).intersects(TypeFlags::BIGINT_LIKE)
                }) && t != TypeId::NEVER
                {
                    return TypeId::BIGINT;
                }
                TypeId::NUMBER
            }
            UnaryOp::TypeOf => {
                self.check_expr(al);
                self.typeof_result_type()
            }
            UnaryOp::Void => {
                self.check_expr(al);
                TypeId::UNDEFINED
            }
            UnaryOp::Delete => {
                self.check_expr(al);
                let ast = self.ast(e.file);
                let inner = skip_parens(ast, arg);
                match *ast.node(inner) {
                    Node::Member { .. } | Node::Index { .. } => {
                        let il = Loc::new(e.file, inner);
                        let prop = self.property_symbol_of_access(il);
                        // Deleting through a readonly index signature (`delete v[2]` of a
                        // readonly tuple's rest) writes it: TS2542.
                        if prop.is_none() && self.check_readonly_index_write(il) {
                            return TypeId::BOOLEAN;
                        }
                        if self.c.opts.strict_null_checks
                            && let Some(p) = prop
                        {
                            let pt = self.type_of_symbol(p);
                            let optional = self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL);
                            let any_like = self
                                .store()
                                .flags(pt)
                                .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN | TypeFlags::NEVER);
                            let has_undef = self.nullable_facts(pt).0;
                            if !optional && !any_like && !has_undef {
                                self.error(
                                    al,
                                    dg::THE_OPERAND_OF_A_DELETE_OPERATOR_MUST_BE_OPTIONAL_2790,
                                    &[],
                                );
                            }
                        }
                    }
                    _ => {
                        self.error(
                            al,
                            dg::THE_OPERAND_OF_A_DELETE_OPERATOR_MUST_BE_A_PROPERTY_REFERENCE_2703,
                            &[],
                        );
                    }
                }
                TypeId::BOOLEAN
            }
        }
    }

    /// The property symbol a member access refers to (quietly).
    pub fn property_symbol_of_access(&mut self, e: Loc) -> Option<SymId> {
        let ast = self.ast(e.file);
        match *ast.node(e.node) {
            Node::Member { obj, prop, .. } => {
                let ot = self.quietly(|ck| ck.check_expr(Loc::new(e.file, obj)));
                let name = match *ast.node(prop) {
                    Node::Ident { name } => ast.atom(name).to_owned(),
                    Node::PrivateName { name } => format!("#{}", ast.atom(name)),
                    _ => return None,
                };
                let nt = self.non_nullable(ot);
                self.property_of_type(nt, &name)
            }
            Node::Index { obj, index, .. } => {
                let ot = self.quietly(|ck| ck.check_expr(Loc::new(e.file, obj)));
                let it = self.quietly(|ck| ck.check_expr(Loc::new(e.file, index)));
                let name = self.property_name_of_type(self.store().regular(it))?;
                let nt = self.non_nullable(ot);
                self.property_of_type(nt, &name)
            }
            _ => None,
        }
    }

    /// `"string" | "number" | "bigint" | "boolean" | "symbol" | "undefined" | "object" | "function"`.
    pub fn typeof_result_type(&mut self) -> TypeId {
        let s = self.store();
        let names = [
            "string",
            "number",
            "bigint",
            "boolean",
            "symbol",
            "undefined",
            "object",
            "function",
        ];
        let types: Vec<TypeId> = names.iter().map(|n| s.string_literal(n, false)).collect();
        self.union(&types)
    }

    fn check_update(&mut self, e: Loc, op: UpdateOp, arg: NodeId) -> TypeId {
        let _ = op;
        let al = Loc::new(e.file, arg);
        let t = self.check_expr(al);
        // `++undefined` reports only that `undefined` is not a variable (observed).
        let ast = self.ast(e.file);
        if let Node::Ident { name } = *ast.node(skip_parens(ast, arg))
            && ast.atom(name) == "undefined"
            && self.store().flags(t).intersects(TypeFlags::UNDEFINED)
        {
            self.error(
                al,
                dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_NOT_A_VARIABLE_2539,
                &["undefined"],
            );
            return TypeId::NUMBER;
        }
        let t = self.check_non_null(al, t);
        // A target that can't be assigned (`++E` for an enum, `eval++`) reports only that;
        // an operand that isn't a number reports only that (`++true`, not TS2357 too).
        if !self.check_reference_target(al, false) {
            return TypeId::NUMBER;
        }
        let nb = self.number_or_bigint();
        let ok = self.is_assignable(t, nb);
        if !ok {
            self.error(
                al,
                dg::AN_ARITHMETIC_OPERAND_MUST_BE_OF_TYPE_ANY_NUMBER_BIGINT_OR_AN_ENUM_TYPE_2356,
                &[],
            );
            // A readonly index signature target is reported too (`Enum[k]--`).
            self.check_readonly_index_write(al);
        } else if self.check_reference_target(al, true) && !self.check_readonly_index_write(al) {
            self.check_readonly_update_target(al);
        }
        if self.every_member(t, |m| {
            self.store().flags(m).intersects(TypeFlags::BIGINT_LIKE)
        }) && t != TypeId::NEVER
        {
            TypeId::BIGINT
        } else {
            TypeId::NUMBER
        }
    }

    /// TS2542 for a write through a readonly index signature by the element access `il`
    /// that reads no property (`delete v[2]`, `ro[k]++`, `Enum[k]--`); whether reported.
    fn check_readonly_index_write(&mut self, il: Loc) -> bool {
        let ast = self.ast(il.file);
        let Node::Index { obj, index, .. } = *ast.node(skip_parens(ast, il.node)) else {
            return false;
        };
        if self.property_symbol_of_access(il).is_some() {
            return false;
        }
        let ot = self.quietly(|ck| ck.check_expr(Loc::new(il.file, obj)));
        let it = self.quietly(|ck| ck.check_expr(Loc::new(il.file, index)));
        let nt = self.non_nullable(ot);
        let f = self.store().flags(it);
        let info = if f.intersects(TypeFlags::ANY) {
            // An `any` key (or an unresolved one) prefers the numeric index signature.
            self.index_info_of(nt, TypeId::NUMBER)
                .or_else(|| self.index_info_of(nt, TypeId::STRING))
        } else if f.intersects(TypeFlags::NUMBER_LIKE) {
            self.index_info_of(nt, TypeId::NUMBER)
        } else {
            self.index_info_of(nt, TypeId::STRING)
        };
        if let Some(i) = info
            && i.readonly
        {
            let ts = self.type_to_string(nt);
            self.error(
                il,
                dg::INDEX_SIGNATURE_IN_TYPE_0_ONLY_PERMITS_READING_2542,
                &[&ts],
            );
            return true;
        }
        false
    }

    /// TS2540 for `o.p++` / `--o.p` on a read-only property.
    fn check_readonly_update_target(&mut self, al: Loc) {
        let ast = self.ast(al.file);
        let mut n = skip_parens(ast, al.node);
        for _ in 0..64 {
            match *ast.node(n) {
                Node::NonNull { expr } => n = skip_parens(ast, expr),
                _ => break,
            }
        }
        let (obj, key, name) = match *ast.node(n) {
            Node::Member { obj, prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => (obj, prop, ast.atom(name).to_owned()),
                _ => return,
            },
            Node::Index { obj, index, .. } => match *ast.node(skip_parens(ast, index)) {
                Node::StringLit { value } => (obj, index, ast.atom(value).to_owned()),
                Node::NumberLit { value } => {
                    (obj, index, ts_r_binder::js_number_to_string(value.get()))
                }
                _ => return,
            },
            _ => return,
        };
        let prop = key;
        let ot = self.check_expr(Loc::new(al.file, obj));
        if ot == TypeId::UNRESOLVED || self.store().is_error(ot) {
            return;
        }
        let nt = self.quietly(|ck| ck.check_non_null(Loc::new(al.file, obj), ot));
        let name_id = self.store().intern_str(&name);
        let Some(p) = self.property_of_type_id(nt, name_id) else {
            return;
        };
        let nl = Loc::new(al.file, n);
        if self.is_readonly_write(nl, p) {
            self.error(
                Loc::new(al.file, prop),
                dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_READ_ONLY_PROPERTY_2540,
                &[&name],
            );
        }
    }

    fn number_or_bigint(&mut self) -> TypeId {
        self.union(&[TypeId::NUMBER, TypeId::BIGINT])
    }

    /// Validates an assignment target: a variable or property, not a constant, function,
    /// class, enum, namespace or import. `update` selects the `++`/`--` message. Returns
    /// whether the target is valid (an invalid one is not checked for assignability).
    fn check_reference_target(&mut self, l: Loc, update: bool) -> bool {
        let ast = self.ast(l.file);
        // `x!++` and `(x!) = 1` still check `x` (constants included).
        let mut n = skip_parens(ast, l.node);
        for _ in 0..64 {
            match *ast.node(n) {
                Node::NonNull { expr } => n = skip_parens(ast, expr),
                _ => break,
            }
        }
        match *ast.node(n) {
            Node::Ident { name } => {
                let text = ast.atom(name);
                let Some(s) = self.c.resolve_ident(Loc::new(l.file, n), meaning::VALUE) else {
                    return true;
                };
                let f = self.c.sym_flags(s);
                let info = if f.intersects(SymbolFlags::ALIAS) {
                    Some(dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_AN_IMPORT_2632)
                } else if f.intersects(SymbolFlags::FUNCTION) {
                    Some(dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_FUNCTION_2630)
                } else if f.intersects(SymbolFlags::CLASS) {
                    Some(dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_CLASS_2629)
                } else if f.intersects(SymbolFlags::ENUM) {
                    Some(dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_AN_ENUM_2628)
                } else if f.intersects(SymbolFlags::VALUE_MODULE)
                    && !f.intersects(SymbolFlags::VARIABLE)
                {
                    Some(dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_NAMESPACE_2631)
                } else if f.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                    && let Some(d) = self.c.value_decl(s)
                    && self.is_const_declaration(d)
                {
                    Some(dg::CANNOT_ASSIGN_TO_0_BECAUSE_IT_IS_A_CONSTANT_2588)
                } else {
                    None
                };
                if let Some(i) = info {
                    self.error(Loc::new(l.file, n), i, &[text]);
                    return false;
                }
                true
            }
            // `a?.b++` (tsc 7.0.2).
            Node::Member { .. } | Node::Index { .. }
                if update && self.in_optional_chain(Loc::new(l.file, n)) =>
            {
                self.error(l, dg::THE_OPERAND_OF_AN_INCREMENT_OR_DECREMENT_OPERATOR_MAY_NOT_BE_AN_OPTIONAL_PROPERT_2777, &[]);
                true
            }
            // A missing operand is already a syntax error (`++\n++y`, `++await 1`).
            Node::Member { .. } | Node::Index { .. } | Node::Missing {} => true,
            _ => {
                // `(x as number)++`, `(x satisfies number)++` are references (and not
                // checked for constants).
                let mut inner = n;
                for _ in 0..64 {
                    match *ast.node(inner) {
                        Node::NonNull { expr }
                        | Node::As { expr, .. }
                        | Node::Satisfies { expr, .. }
                        | Node::TypeAssertion { expr, .. }
                        | Node::Paren { expr } => inner = expr,
                        _ => break,
                    }
                }
                if inner != n
                    && matches!(
                        ast.node(inner),
                        Node::Ident { .. } | Node::Member { .. } | Node::Index { .. }
                    )
                {
                    return true;
                }
                if update {
                    self.error(l, dg::THE_OPERAND_OF_AN_INCREMENT_OR_DECREMENT_OPERATOR_MUST_BE_A_VARIABLE_OR_A_PROPER_2357, &[]);
                }
                true
            }
        }
    }

    fn check_binary(&mut self, e: Loc, left: NodeId, op: BinaryOp, right: NodeId) -> TypeId {
        let f = e.file;
        let ll = Loc::new(f, left);
        let rl = Loc::new(f, right);
        match op {
            BinaryOp::LogicalAnd => {
                let lt = self.check_truthiness_expr(ll);
                let rt = self.check_expr(rl);
                if self.truthiness(lt).truthy {
                    let base = if self.c.opts.strict_null_checks {
                        lt
                    } else {
                        self.base_of_literal(rt)
                    };
                    let falsy = self.extract_definitely_falsy(base);
                    self.union(&[falsy, rt])
                } else {
                    lt
                }
            }
            BinaryOp::LogicalOr => {
                let lt = self.check_truthiness_expr(ll);
                let rt = self.check_expr(rl);
                if self.truthiness(lt).falsy {
                    let l2 = self.remove_definitely_falsy(lt);
                    let l2 = self.filter_type(l2, |ck, m| {
                        !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
                    });
                    // A generic left side that may be nullish is `NonNullable<T>`.
                    let l2 = self.non_nullable_generic_members(l2);
                    self.union_with(&[l2, rt], Reduction::Subtype, AliasId::NONE)
                } else {
                    lt
                }
            }
            BinaryOp::Coalesce => {
                self.check_nullish_left_shape(ll);
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                let (u, n) = self.nullable_facts(lt);
                let any = self
                    .store()
                    .flags(lt)
                    .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN);
                // A generic left side that may be nullish is `NonNullable<T>` on the left.
                let l2 = self.non_nullable(lt);
                if u || n || any || !self.c.opts.strict_null_checks || l2 != lt {
                    self.union_with(&[l2, rt], Reduction::Subtype, AliasId::NONE)
                } else {
                    lt
                }
            }
            BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                if !self.equality_comparable(lt, rt) && !self.equality_comparable(rt, lt) {
                    self.report_operator_error(e, op.as_str(), lt, rt, true);
                }
                // An object, array, function, class or regex literal operand (not
                // parenthesized) is a new object.
                let ast = self.ast(e.file);
                let literal = |n: NodeId| {
                    matches!(
                        ast.node(n),
                        Node::ObjectLit { .. }
                            | Node::ArrayLit { .. }
                            | Node::FunctionExpr { .. }
                            | Node::ClassExpr { .. }
                            | Node::RegexLit { .. }
                    )
                };
                // In JavaScript only `===` and `!==` (observed: `{} == {}` is fine there).
                let loose = matches!(op, BinaryOp::Eq | BinaryOp::NotEq);
                if (literal(ll.node) || literal(rl.node))
                    && self.reporting(e.file)
                    && !(loose && self.c.file(e.file).is_js())
                {
                    let always = if matches!(op, BinaryOp::Eq | BinaryOp::StrictEq) {
                        "false"
                    } else {
                        "true"
                    };
                    self.error(
                        e,
                        dg::THIS_CONDITION_WILL_ALWAYS_RETURN_0_SINCE_JAVASCRIPT_COMPARES_OBJECTS_BY_REFEREN_2839,
                        &[always],
                    );
                }
                TypeId::BOOLEAN
            }
            BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                let lt = self.check_non_null(ll, lt);
                let rt = self.check_non_null(rl, rt);
                if self.store().is_error(lt) || self.store().is_error(rt) {
                    return TypeId::BOOLEAN;
                }
                let lb = self.base_for_comparison(lt);
                let rb = self.base_for_comparison(rt);
                let anyish = |ck: &Self, t: TypeId| ck.store().flags(t).intersects(TypeFlags::ANY);
                if !anyish(self, lb) && !anyish(self, rb) {
                    let nb = self.number_or_bigint();
                    let la = self.is_assignable(lb, nb);
                    let ra = self.is_assignable(rb, nb);
                    let ok = (la && ra)
                        || (!la
                            && !ra
                            && (self.is_comparable(lb, rb) || self.is_comparable(rb, lb)));
                    if !ok {
                        self.report_operator_error(e, op.as_str(), lb, rb, false);
                    }
                }
                TypeId::BOOLEAN
            }
            BinaryOp::In => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                let lt = self.check_non_null(ll, lt);
                let rt = self.check_non_null(rl, rt);
                if !self.store().is_error(lt) {
                    let key = self.keyof_constraint();
                    if !self.is_assignable(lt, key) {
                        let ls = self.type_to_string(lt);
                        let ks = self.type_to_string(key);
                        let _ = (ls, ks);
                        self.check_type_related(lt, key, RelationKind::Assignable, Some(ll), None);
                    }
                }
                // The right operand must be an `object` (TS2322); the `{}` of a narrowed
                // `unknown` (alone or as `NonNullable<T>`) may be a primitive (TS2638).
                let st = self.store();
                // (`unknown` was TS18046 under strictNullChecks.)
                let reported_unknown = rt == TypeId::UNKNOWN && self.c.opts.strict_null_checks;
                if !st.is_error(rt) && !rt.is_any_like() && !reported_unknown {
                    let narrowed_empty = self.some_member(rt, |m| {
                        m == TypeId::EMPTY_OBJECT
                            || (st.flags(m).contains(TypeFlags::INTERSECTION)
                                && st.members_of_union(m).contains(&TypeId::EMPTY_OBJECT))
                    });
                    if narrowed_empty {
                        let rs = self.type_to_string(rt);
                        self.error(rl, dg::TYPE_0_MAY_REPRESENT_A_PRIMITIVE_VALUE_WHICH_IS_NOT_PERMITTED_AS_THE_RIGHT_OPERA_2638, &[&rs]);
                    } else if !self.is_assignable(rt, TypeId::OBJECT) {
                        self.check_type_related(
                            rt,
                            TypeId::OBJECT,
                            RelationKind::Assignable,
                            Some(rl),
                            None,
                        );
                    }
                }
                TypeId::BOOLEAN
            }
            BinaryOp::InstanceOf => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                let lf = self.store().flags(lt);
                if !lf.intersects(TypeFlags::ANY)
                    && !self.store().is_error(lt)
                    && !self.could_be_object(lt)
                {
                    self.error(ll, dg::THE_LEFT_HAND_SIDE_OF_AN_INSTANCEOF_EXPRESSION_MUST_BE_OF_TYPE_ANY_AN_OBJECT_TYP_2358, &[]);
                }
                if !self.store().flags(rt).intersects(TypeFlags::ANY) && !self.store().is_error(rt)
                {
                    let callable = !self
                        .signatures_of(rt, crate::members::SigKind::Construct)
                        .is_empty()
                        || !self
                            .signatures_of(rt, crate::members::SigKind::Call)
                            .is_empty()
                        || self
                            .global_type(GlobalType::Function)
                            .is_some_and(|ft| self.is_assignable(rt, ft))
                        || self.property_of_type(rt, "__@hasInstance@0").is_some();
                    if !callable {
                        self.error(rl, dg::THE_RIGHT_HAND_SIDE_OF_AN_INSTANCEOF_EXPRESSION_MUST_BE_EITHER_OF_TYPE_ANY_A_CLA_2359, &[]);
                    }
                }
                TypeId::BOOLEAN
            }
            BinaryOp::Add => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                self.check_plus(e, "+", ll, rl, lt, rt)
            }
            _ => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                self.check_arithmetic(e, op.as_str(), op.as_str(), ll, rl, lt, rt)
            }
        }
    }

    fn could_be_object(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            return ms.into_iter().any(|m| self.could_be_object(m));
        }
        if f.intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(t);
            if c == TypeId::UNKNOWN || c == t || c == TypeId::UNRESOLVED {
                return true;
            }
            return self.could_be_object(c);
        }
        f.intersects(
            TypeFlags::OBJECT
                | TypeFlags::NON_PRIMITIVE
                | TypeFlags::INTERSECTION
                | TypeFlags::ANY
                | TypeFlags::UNKNOWN,
        )
    }

    fn equality_comparable(&mut self, s: TypeId, t: TypeId) -> bool {
        if self.store().flags(t).intersects(TypeFlags::NULLABLE) {
            return true;
        }
        if s.is_any_like()
            || t.is_any_like()
            || self.store().is_error(s)
            || self.store().is_error(t)
        {
            return true;
        }
        self.is_comparable(s, t)
    }

    fn report_operator_error(&mut self, e: Loc, op: &str, lt: TypeId, rt: TypeId, equality: bool) {
        if !self.reporting(e.file) {
            return;
        }
        // Literal operands of unrelated kinds are shown widened (`number` and `string`, not
        // `1` and `"x"`); of one kind they are kept (`"a"` and `"b"`).
        let (lt, rt) = if equality {
            let lb = self.base_of_literal(lt);
            let rb = self.base_of_literal(rt);
            if (lb != lt || rb != rt) && !self.is_comparable(lb, rb) && !self.is_comparable(rb, lb)
            {
                (lb, rb)
            } else {
                (lt, rt)
            }
        } else {
            (lt, rt)
        };
        let ls = self.type_to_string(lt);
        let rs = self.type_to_string(rt);
        if equality {
            self.error(e, dg::THIS_COMPARISON_APPEARS_TO_BE_UNINTENTIONAL_BECAUSE_THE_TYPES_0_AND_1_HAVE_NO_OV_2367, &[&ls, &rs]);
        } else {
            self.error(
                e,
                dg::OPERATOR_0_CANNOT_BE_APPLIED_TO_TYPES_1_AND_2_2365,
                &[op, &ls, &rs],
            );
        }
    }

    fn assignable_to_kind(&mut self, t: TypeId, kind: TypeFlags, strict: bool) -> bool {
        let f = self.store().flags(t);
        if f.intersects(kind) {
            return true;
        }
        if strict
            && f.intersects(
                TypeFlags::ANY
                    | TypeFlags::UNKNOWN
                    | TypeFlags::VOID
                    | TypeFlags::UNDEFINED
                    | TypeFlags::NULL,
            )
        {
            return false;
        }
        (kind.intersects(TypeFlags::NUMBER_LIKE) && self.is_assignable(t, TypeId::NUMBER))
            || (kind.intersects(TypeFlags::BIGINT_LIKE) && self.is_assignable(t, TypeId::BIGINT))
            || (kind.intersects(TypeFlags::STRING_LIKE) && self.is_assignable(t, TypeId::STRING))
    }

    /// `+` (or `+=`, named by `shown` in messages).
    fn check_plus(
        &mut self,
        e: Loc,
        shown: &str,
        ll: Loc,
        rl: Loc,
        lt: TypeId,
        rt: TypeId,
    ) -> TypeId {
        let (mut lt, mut rt) = (lt, rt);
        if lt == TypeId::UNRESOLVED || rt == TypeId::UNRESOLVED {
            return TypeId::UNRESOLVED;
        }
        if !self.assignable_to_kind(lt, TypeFlags::STRING_LIKE, false)
            && !self.assignable_to_kind(rt, TypeFlags::STRING_LIKE, false)
        {
            lt = self.check_non_null(ll, lt);
            rt = self.check_non_null(rl, rt);
        }
        if self.store().is_error(lt) || self.store().is_error(rt) {
            // A string operand makes the result `string` even next to the error type
            // (`Missing + 's'`); otherwise the error type propagates (`Missing + 1`).
            let other = if self.store().is_error(lt) { rt } else { lt };
            if !self.store().is_error(other)
                && self.assignable_to_kind(other, TypeFlags::STRING_LIKE, true)
            {
                return TypeId::STRING;
            }
            return TypeId::ERROR;
        }
        if self.assignable_to_kind(lt, TypeFlags::NUMBER_LIKE, true)
            && self.assignable_to_kind(rt, TypeFlags::NUMBER_LIKE, true)
        {
            return TypeId::NUMBER;
        }
        if self.assignable_to_kind(lt, TypeFlags::BIGINT_LIKE, true)
            && self.assignable_to_kind(rt, TypeFlags::BIGINT_LIKE, true)
        {
            return TypeId::BIGINT;
        }
        if self.assignable_to_kind(lt, TypeFlags::STRING_LIKE, true)
            || self.assignable_to_kind(rt, TypeFlags::STRING_LIKE, true)
        {
            return TypeId::STRING;
        }
        if self.store().flags(lt).intersects(TypeFlags::ANY)
            || self.store().flags(rt).intersects(TypeFlags::ANY)
        {
            return TypeId::ANY;
        }
        // Mixing number and bigint (or a `null`/`undefined` operand without strict null
        // checks) shows the operand types as they are; any other invalid pair shows them
        // widened (`1 + {}` reports 'number' and '{}').
        let numeric = TypeFlags::NUMBER_LIKE | TypeFlags::BIGINT_LIKE;
        let nullish = |ck: &Self, t: TypeId| {
            ck.store()
                .flags(t)
                .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED)
        };
        let both_numeric = self.assignable_to_kind(lt, numeric, true)
            && self.assignable_to_kind(rt, numeric, true);
        let keep = both_numeric || nullish(self, lt) || nullish(self, rt);
        if !keep {
            lt = self.base_of_literal(lt);
            rt = self.base_of_literal(rt);
        }
        self.report_operator_error(e, shown, lt, rt, false);
        TypeId::ANY
    }

    /// Arithmetic and bitwise operators; `shown` names the operator in TS2365 (`-=` for a
    /// compound assignment).
    #[allow(clippy::too_many_arguments)]
    fn check_arithmetic(
        &mut self,
        e: Loc,
        op: &str,
        shown: &str,
        ll: Loc,
        rl: Loc,
        lt: TypeId,
        rt: TypeId,
    ) -> TypeId {
        if lt == TypeId::UNRESOLVED || rt == TypeId::UNRESOLVED {
            return TypeId::UNRESOLVED;
        }
        let (l0, r0) = (lt, rt);
        let lt = self.check_non_null(ll, lt);
        let rt = self.check_non_null(rl, rt);
        if self.store().is_error(lt) || self.store().is_error(rt) {
            // An operand of the error type itself (an unresolved name) is an `any`:
            // `Missing * 7` is a `number` (a `bigint` next to an all-bigint operand), and
            // nothing is reported. An operand made an error here (`null`, `unknown`) was
            // reported already; the result stays the error type.
            if (self.store().is_error(lt) && !self.store().is_error(l0))
                || (self.store().is_error(rt) && !self.store().is_error(r0))
            {
                return TypeId::ERROR;
            }
            let other = if self.store().is_error(lt) { rt } else { lt };
            if !self.store().is_error(other) {
                // An invalid other operand (`true * Missing`) is not modelled; stay silent.
                let nb = self.number_or_bigint();
                if !self.is_assignable(other, nb) {
                    return TypeId::ERROR;
                }
            }
            let all_big = !self.store().is_error(other)
                && self.every_member(other, |m| {
                    self.store().flags(m).intersects(TypeFlags::BIGINT_LIKE)
                });
            return if all_big {
                TypeId::BIGINT
            } else {
                TypeId::NUMBER
            };
        }
        let bitwise = matches!(op, "|" | "&" | "^");
        let boolish = |ck: &Self, t: TypeId| {
            ck.every_member(t, |m| {
                ck.store().flags(m).intersects(TypeFlags::BOOLEAN_LIKE)
            })
        };
        if bitwise && boolish(self, lt) && boolish(self, rt) {
            let sugg = match op {
                "|" => "||",
                "&" => "&&",
                _ => "!==",
            };
            self.error(
                e,
                dg::THE_0_OPERATOR_IS_NOT_ALLOWED_FOR_BOOLEAN_TYPES_CONSIDER_USING_1_INSTEAD_2447,
                &[op, sugg],
            );
            return TypeId::NUMBER;
        }
        let nb = self.number_or_bigint();
        let lok = self.is_assignable(lt, nb);
        if !lok {
            self.error(ll, dg::THE_LEFT_HAND_SIDE_OF_AN_ARITHMETIC_OPERATION_MUST_BE_OF_TYPE_ANY_NUMBER_BIGINT_O_2362, &[]);
        }
        let rok = self.is_assignable(rt, nb);
        if !rok {
            self.error(rl, dg::THE_RIGHT_HAND_SIDE_OF_AN_ARITHMETIC_OPERATION_MUST_BE_OF_TYPE_ANY_NUMBER_BIGINT_2363, &[]);
        }
        let any = |ck: &Self, t: TypeId| ck.store().flags(t).intersects(TypeFlags::ANY);
        let maybe_big = |ck: &Self, t: TypeId| {
            ck.some_member(t, |m| {
                ck.store().flags(m).intersects(TypeFlags::BIGINT_LIKE)
            })
        };
        if (any(self, lt) && any(self, rt)) || !(maybe_big(self, lt) || maybe_big(self, rt)) {
            return TypeId::NUMBER;
        }
        let all_big = |ck: &Self, t: TypeId| {
            ck.every_member(t, |m| {
                ck.store()
                    .flags(m)
                    .intersects(TypeFlags::BIGINT_LIKE | TypeFlags::ANY)
            })
        };
        if all_big(self, lt) && all_big(self, rt) {
            return TypeId::BIGINT;
        }
        if lok && rok {
            // Operand types are shown widened (`bigint` and `number`, not `2`).
            let lb = self.base_of_literal(lt);
            let rb = self.base_of_literal(rt);
            self.report_operator_error(e, shown, lb, rb, false);
        }
        TypeId::ERROR
    }

    // ---------------------------------------------------------------------------------
    // Assignment
    // ---------------------------------------------------------------------------------

    /// The declared (not narrowed) type of an assignment target, reporting invalid targets.
    pub fn assignment_target_type(&mut self, l: Loc) -> TypeId {
        let ast = self.ast(l.file);
        let n = skip_parens(ast, l.node);
        let nl = Loc::new(l.file, n);
        match *ast.node(n) {
            Node::Ident { name } => {
                let text = ast.atom(name);
                let Some(s) = self.c.resolve_ident(nl, meaning::VALUE) else {
                    return self.unresolved_identifier(nl, text);
                };
                self.checked.insert(nl);
                let s2 = if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                    self.c.resolve_alias(s).unwrap_or(s)
                } else {
                    s
                };
                self.check_use_before_declaration(nl, s2, text);
                self.check_parameter_reference(nl, s2, text);
                let t = self.type_of_symbol(s2);
                if t == TypeId::AUTO { TypeId::ANY } else { t }
            }
            Node::Member { obj, prop, .. } => {
                let ot = self.check_expr(Loc::new(l.file, obj));
                self.property_access(nl, obj, ot, prop, true)
            }
            Node::Index { obj, index, .. } => {
                let ot = self.check_expr(Loc::new(l.file, obj));
                self.element_access(nl, obj, ot, index, true)
            }
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => {
                self.check_destructuring_targets(nl);
                TypeId::UNRESOLVED
            }
            _ => self.check_expr(nl),
        }
    }

    pub(crate) fn check_destructuring_targets(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Property { key, value, .. } => {
                            if let Node::ComputedKey { expr } = *ast.node(key) {
                                self.check_expr(Loc::new(l.file, expr));
                            }
                            self.destructuring_target(Loc::new(l.file, value));
                        }
                        Node::ShorthandProp { name, init, .. } => {
                            let nl = Loc::new(l.file, name);
                            let tt = self.assignment_target_type(nl);
                            if let Some(i) = init {
                                // `{ a = v }`: the default must fit the target.
                                let rt = self.check_expr(Loc::new(l.file, i));
                                if !self.store().is_error(tt) && tt != TypeId::UNRESOLVED {
                                    self.check_assignable_with_elaboration(
                                        rt,
                                        tt,
                                        nl,
                                        Some(Loc::new(l.file, i)),
                                    );
                                }
                            }
                        }
                        Node::Spread { arg } => {
                            let al = Loc::new(l.file, arg);
                            self.check_destructuring_element_shape(al, true);
                            self.assignment_target_type(al);
                        }
                        _ => {}
                    }
                }
            }
            Node::ArrayLit { elements } => {
                for &el in ast.list(elements) {
                    match *ast.node(el) {
                        Node::Spread { arg } => self.destructuring_target(Loc::new(l.file, arg)),
                        Node::Hole {} => {}
                        _ => self.destructuring_target(Loc::new(l.file, el)),
                    }
                }
            }
            _ => {}
        }
    }

    /// `[a, b] = v`, `({ a, b: c } = v)`, `for ([a] of xs)`: every target is checked, and
    /// the part of `source` it receives must be assignable to it (TS2322 at the target).
    /// Rest elements, missing properties and out-of-range tuple elements are only checked
    /// as targets.
    pub(crate) fn check_destructuring_assignment(&mut self, l: Loc, source: TypeId, depth: u32) {
        self.check_destructuring_assignment_from(l, source, depth, None);
    }

    /// [`Self::check_destructuring_assignment`] with the reference the source is read from
    /// and the path to it (`[a, b] = o` reads `o[0]` and `o[1]` narrowed where `o` is read).
    fn check_destructuring_assignment_from(
        &mut self,
        l: Loc,
        source: TypeId,
        depth: u32,
        origin: Option<(Loc, Vec<Box<str>>)>,
    ) {
        let st = self.store();
        if depth > 32
            || source == TypeId::UNRESOLVED
            || st.is_error(source)
            || st.flags(source).intersects(TypeFlags::ANY)
        {
            self.check_destructuring_targets(l);
            return;
        }
        let f = l.file;
        let ast = self.ast(f);
        match *ast.node(l.node) {
            Node::ArrayLit { elements } => {
                // A source that can't be iterated is TS2488 at the pattern, and its
                // elements are only checked as targets. Not in files with syntax errors,
                // where recovery can make patterns tsc doesn't see.
                let sf = self.c.file(l.file);
                let syntax_errors = sf.syntax.len() > sf.grammar.len();
                if !syntax_errors && self.check_iterable(source, l, false) {
                    self.check_destructuring_targets(l);
                    return;
                }
                let els = ast.list(elements).to_vec();
                let tuple_len = self
                    .store()
                    .tuple_shape_of(source)
                    .filter(|sh| !sh.has_rest())
                    .map(|sh| sh.elements.len());
                for (i, el) in els.into_iter().enumerate() {
                    let ast = self.ast(f);
                    match *ast.node(el) {
                        Node::Hole {} => {}
                        // `[...{ a }] = t`: the nested pattern reads the rest of the source.
                        // A nested array pattern only for tuple sources (an array literal
                        // source is then a tuple typed by the pattern in tsc).
                        Node::Spread { arg }
                            if matches!(*ast.node(arg), Node::ObjectLit { .. })
                                || (matches!(*ast.node(arg), Node::ArrayLit { .. })
                                    && self.store().tuple_shape_of(source).is_some()) =>
                        {
                            let rest = self.array_rest_type(source, i);
                            self.check_destructuring_assignment_from(
                                Loc::new(f, arg),
                                rest,
                                depth + 1,
                                None,
                            );
                        }
                        // `[a, ...b] = xs`: `b` receives the rest of the source as an array.
                        // (Not for a nested pattern, nor when an array literal value meets
                        // an array target: tsc types the literal by the target then.)
                        Node::Spread { arg }
                            if !matches!(
                                *ast.node(skip_parens(ast, arg)),
                                Node::Assign { .. }
                                    | Node::ArrayLit { .. }
                                    | Node::ObjectLit { .. }
                            ) && origin.is_some()
                                && self.is_array_like_or_iterable_source(source)
                                && !(origin.as_ref().is_some_and(|(r, _)| {
                                    matches!(
                                        *self
                                            .ast(r.file)
                                            .node(skip_parens(self.ast(r.file), r.node)),
                                        Node::ArrayLit { .. }
                                    )
                                }) && {
                                    let tt = self
                                        .quietly(|ck| ck.assignment_target_type(Loc::new(f, arg)));
                                    self.store().tuple_shape_of(tt).is_some()
                                        || self.element_type_of_array_like(tt).is_some()
                                }) =>
                        {
                            let rest = self.array_rest_type(source, i);
                            let rest = if self
                                .c
                                .type_args_of(rest)
                                .first()
                                .is_some_and(|&a| a == TypeId::UNRESOLVED)
                            {
                                TypeId::UNRESOLVED
                            } else {
                                rest
                            };
                            self.destructuring_element(Loc::new(f, arg), rest, depth, None);
                        }
                        Node::Spread { arg } => self.destructuring_target(Loc::new(f, arg)),
                        _ => {
                            let sub = origin.as_ref().map(|(r, p)| {
                                let mut p = p.clone();
                                p.push(i.to_string().into());
                                (*r, p)
                            });
                            // Past the end of the tuple (TS2493), unless defaulted.
                            let defaulted = matches!(
                                *ast.node(el),
                                Node::Assign {
                                    op: AssignOp::Assign,
                                    ..
                                }
                            );
                            if !defaulted && self.reporting(f) {
                                self.check_tuple_element_bound(source, i, Loc::new(f, el));
                            }
                            let et = if tuple_len.is_some_and(|n| i >= n) {
                                TypeId::UNRESOLVED
                            } else {
                                let et = self.array_element_at(source, i);
                                match &sub {
                                    Some((r, p)) => self.narrowed_path_type(*r, p.clone(), et),
                                    None => et,
                                }
                            };
                            self.destructuring_element(Loc::new(f, el), et, depth, sub);
                        }
                    }
                }
            }
            Node::ObjectLit { props } => {
                // An empty pattern still needs an object (`({} = maybeNull)`).
                if ast.list(props).is_empty() {
                    self.check_non_null(l, source);
                }
                for p in ast.list(props).to_vec() {
                    let ast = self.ast(f);
                    match *ast.node(p) {
                        Node::Property { key, value, .. } => {
                            if let Node::ComputedKey { expr } = *ast.node(key) {
                                self.check_expr(Loc::new(f, expr));
                            }
                            let name = self.member_key_name(Loc::new(f, key));
                            let sub = match (&origin, &name) {
                                (Some((r, p)), Some(n)) => {
                                    let mut p = p.clone();
                                    p.push(n.as_str().into());
                                    Some((*r, p))
                                }
                                _ => None,
                            };
                            let has_default = matches!(
                                *ast.node(value),
                                Node::Assign {
                                    op: AssignOp::Assign,
                                    ..
                                }
                            );
                            if !has_default && let Node::NumberLit { value } = *ast.node(key) {
                                self.check_tuple_element_in_range(
                                    Loc::new(f, key),
                                    source,
                                    value.get(),
                                );
                            }
                            if let Some(n) = &name {
                                let kl = match *ast.node(key) {
                                    Node::ComputedKey { expr } => Loc::new(f, expr),
                                    _ => Loc::new(f, key),
                                };
                                self.check_destructured_assignment_property(
                                    source,
                                    n,
                                    kl,
                                    has_default,
                                );
                            }
                            let et = match name {
                                Some(name) => {
                                    let et = self.destructured_property_type(source, &name);
                                    match &sub {
                                        Some((r, p)) => self.narrowed_path_type(*r, p.clone(), et),
                                        None => et,
                                    }
                                }
                                None => TypeId::UNRESOLVED,
                            };
                            self.destructuring_element(Loc::new(f, value), et, depth, sub);
                        }
                        Node::ShorthandProp { name, init, .. } => {
                            let nl = Loc::new(f, name);
                            let Node::Ident { name: atom } = *ast.node(name) else {
                                continue;
                            };
                            let text = ast.atom(atom).to_owned();
                            self.check_destructured_assignment_property(
                                source,
                                &text,
                                nl,
                                init.is_some(),
                            );
                            let tt = self.assignment_target_type(nl);
                            let target_ok = !self.store().is_error(tt) && tt != TypeId::UNRESOLVED;
                            let mut et = self.destructured_property_type(source, &text);
                            if let Some((r, p)) = &origin {
                                let mut p = p.clone();
                                p.push(text.as_str().into());
                                et = self.narrowed_path_type(*r, p, et);
                            }
                            if let Some(i) = init {
                                // `{ a = v }`: the default must fit the target.
                                let rt = self.check_expr(Loc::new(f, i));
                                if target_ok {
                                    self.check_assignable_with_elaboration(
                                        rt,
                                        tt,
                                        nl,
                                        Some(Loc::new(f, i)),
                                    );
                                }
                                // A default that may be `undefined` itself leaves the read
                                // value's `undefined` in (`({ x = undefined } = a)` reports
                                // `number | undefined`; observed, tsc 7).
                                if !self.may_be_undefined_value(rt) {
                                    et = self.without_undefined_strict(et);
                                }
                            }
                            if target_ok && self.destructured_part_checkable(et) {
                                self.check_assignable_with_elaboration(et, tt, nl, None);
                            }
                        }
                        Node::Spread { arg } => {
                            let al = Loc::new(f, arg);
                            self.check_destructuring_element_shape(al, true);
                            self.assignment_target_type(al);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// TS2339 at the key of an object assignment pattern's property that the source lacks
    /// (`({ zz } = o)`): named on the source (a primitive's wrapper, `unknown`). A property
    /// with a default read from a fresh object literal is not (the literal is typed by the
    /// pattern); generic sources are not modelled.
    fn check_destructured_assignment_property(
        &mut self,
        source: TypeId,
        name: &str,
        at: Loc,
        has_default: bool,
    ) {
        if !self.reporting(at.file) || name.starts_with('#') {
            return;
        }
        let s = self.store();
        if s.flags(source)
            .intersects(TypeFlags::ANY | TypeFlags::NEVER | TypeFlags::INSTANTIABLE)
            || s.is_error(source)
            || source == TypeId::UNRESOLVED
        {
            return;
        }
        // A unique symbol key the source lacks (with no symbol index signature) cannot
        // index it (observed: TS2538).
        if name.starts_with("__@") {
            let apparent = self.apparent_type(source);
            if self.store().flags(apparent).contains(TypeFlags::OBJECT)
                && self.property_of_type(apparent, name).is_none()
                && self.index_info_of(apparent, TypeId::SYMBOL).is_none()
            {
                self.error(
                    at,
                    dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538,
                    &["unique symbol"],
                );
            }
            return;
        }
        let fresh = s
            .object_of(source)
            .is_some_and(|o| o.flags.contains(ObjectFlags::FRESH_LITERAL));
        if fresh && has_default {
            return;
        }
        // `options || {}`: the empty literal is typed by the pattern (observed: no error).
        if s.flags(source).intersects(TypeFlags::UNION)
            && s.members_of_union(source).iter().any(|&m| {
                s.object_of(m)
                    .is_some_and(|o| o.flags.contains(ObjectFlags::OBJECT_LITERAL))
            })
        {
            return;
        }
        let apparent = self.apparent_type(source);
        if apparent == TypeId::UNRESOLVED || self.store().flags(apparent).intersects(TypeFlags::ANY)
        {
            return;
        }
        if source != TypeId::UNKNOWN {
            if self.property_of_type(source, name).is_some() {
                return;
            }
            let key_t = if is_numeric_name(name) {
                TypeId::NUMBER
            } else {
                TypeId::STRING
            };
            if self.index_info_of(source, key_t).is_some()
                || (key_t == TypeId::NUMBER && self.index_info_of(source, TypeId::STRING).is_some())
                || self.has_index_for_name(source, name)
            {
                return;
            }
        }
        let sf = self.store().flags(source);
        let shown = if sf.intersects(TypeFlags::PRIMITIVE)
            && !sf.intersects(
                TypeFlags::UNION
                    | TypeFlags::NULL
                    | TypeFlags::UNDEFINED
                    | TypeFlags::UNKNOWN
                    | TypeFlags::NEVER
                    | TypeFlags::VOID,
            ) {
            apparent
        } else {
            self.regular_of(source)
        };
        let ts = if shown == TypeId::OBJECT {
            "{}".to_owned()
        } else {
            self.type_to_string(shown)
        };
        self.error(
            at,
            dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
            &[name, &ts],
        );
    }

    /// One element of a destructuring assignment receiving `et`.
    fn destructuring_element(
        &mut self,
        el: Loc,
        et: TypeId,
        depth: u32,
        origin: Option<(Loc, Vec<Box<str>>)>,
    ) {
        let f = el.file;
        let ast = self.ast(f);
        match *ast.node(el.node) {
            // `[a = v]`: the default is assigned to the target; the target then receives the
            // element without `undefined`.
            Node::Assign {
                left,
                right,
                op: AssignOp::Assign,
            } => {
                let lnode = skip_parens(ast, left);
                let et = self.without_undefined_strict(et);
                if matches!(
                    ast.node(lnode),
                    Node::ObjectLit { .. } | Node::ArrayLit { .. }
                ) {
                    self.check_expr(Loc::new(f, right));
                    self.check_destructuring_assignment_from(
                        Loc::new(f, lnode),
                        et,
                        depth + 1,
                        None,
                    );
                    return;
                }
                self.check_assignment(el, left, AssignOp::Assign, right);
                let tt = self.quietly(|ck| ck.assignment_target_type(Loc::new(f, left)));
                if !self.store().is_error(tt)
                    && tt != TypeId::UNRESOLVED
                    && self.destructured_part_checkable(et)
                {
                    self.check_assignable_with_elaboration(et, tt, Loc::new(f, left), None);
                }
            }
            _ => {
                let n = skip_parens(ast, el.node);
                if matches!(ast.node(n), Node::ObjectLit { .. } | Node::ArrayLit { .. }) {
                    self.check_destructuring_assignment_from(Loc::new(f, n), et, depth + 1, origin);
                    return;
                }
                // Not a reference (`[""] = xs`): TS2364, nothing to relate.
                if !self.check_reference_target(el, false)
                    || !self.check_destructuring_element_shape(el, false)
                {
                    return;
                }
                let tt = self.assignment_target_type(el);
                if !self.store().is_error(tt)
                    && tt != TypeId::UNRESOLVED
                    && self.destructured_part_checkable(et)
                {
                    self.check_assignable_with_elaboration(et, tt, el, None);
                }
            }
        }
    }

    /// Whether destructuring `t` with a rest element gives a known array: a tuple, an
    /// array or another iterable (not a string, `any` or the error type).
    fn is_array_like_or_iterable_source(&mut self, t: TypeId) -> bool {
        let s = self.store();
        if t == TypeId::UNRESOLVED
            || s.is_error(t)
            || s.flags(t)
                .intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::STRING_LIKE)
        {
            return false;
        }
        s.tuple_shape_of(t).is_some()
            || self.element_type_of_array_like(t).is_some()
            || self.iterated_type(t, false).is_some()
    }

    fn destructured_part_checkable(&self, et: TypeId) -> bool {
        et != TypeId::UNRESOLVED && !self.store().is_error(et)
    }

    /// `t` without `undefined` under strictNullChecks (a destructuring default replaces it).
    /// A value of type `t` may be `undefined` (`undefined`, `void`, `any`, `unknown` or a
    /// union holding one).
    fn may_be_undefined_value(&self, t: TypeId) -> bool {
        self.contains_undefined(t)
            || self
                .store()
                .flags(t)
                .intersects(TypeFlags::VOID | TypeFlags::ANY_OR_UNKNOWN)
    }

    pub(crate) fn without_undefined_strict(&mut self, t: TypeId) -> TypeId {
        if self.c.opts.strict_null_checks {
            self.filter_type(t, |ck, m| {
                !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
            })
        } else {
            t
        }
    }

    fn destructuring_target(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            // `[a = v]`: an assignment of the default to the target.
            Node::Assign {
                left,
                right,
                op: AssignOp::Assign,
            } => {
                self.check_assignment(l, left, AssignOp::Assign, right);
            }
            _ => {
                self.check_destructuring_element_shape(l, false);
                self.assignment_target_type(l);
            }
        }
    }

    fn check_assignment(&mut self, e: Loc, left: NodeId, op: AssignOp, right: NodeId) -> TypeId {
        let f = e.file;
        let ll = Loc::new(f, left);
        let rl = Loc::new(f, right);
        let ast = self.ast(f);
        let lnode = skip_parens(ast, left);
        if op == AssignOp::Assign
            && matches!(
                ast.node(lnode),
                Node::ObjectLit { .. } | Node::ArrayLit { .. }
            )
        {
            let rt = self.check_expr(rl);
            let origin = Some((rl, Vec::new()));
            self.check_destructuring_assignment_from(Loc::new(f, lnode), rt, 0, origin);
            return rt;
        }
        // An arithmetic compound assignment checks its operands before the target's shape
        // (TS2362 rather than TS2364 for `this **= v`).
        let arithmetic = !matches!(
            op,
            AssignOp::Assign | AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign
        );
        let valid_target = self.check_reference_target(ll, false)
            && (arithmetic || self.check_assignment_target_shape(ll));
        let target = self.assignment_target_type(ll);
        match op {
            AssignOp::Assign => {
                let rt = self.check_expr(rl);
                // `/** @type {T} */ module.exports = v` (or `exports.x = v`, `F.prototype =
                // v`) in JavaScript: `v` must be a `T`.
                if self.c.file(f).is_js()
                    && !ast.jsdoc_tags.is_empty()
                    && (self.is_commonjs_export_declaration(Loc::new(f, lnode))
                        || matches!(*ast.node(lnode), Node::Member { obj, prop, .. }
                            if ast.name_of(prop) == Some("prototype")
                                || (ast.name_of(prop) == Some("exports")
                                    && matches!(ast.node(obj), Node::Ident { .. })
                                    && ast.name_of(obj) == Some("module"))))
                    && let Some(ty) = ast
                        .jsdoc_tags_of(left)
                        .iter()
                        .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
                        .and_then(|t| t.ty)
                {
                    let declared = self.type_from_node(Loc::new(f, ty));
                    if declared != TypeId::UNRESOLVED && !self.store().is_error(declared) {
                        self.check_assignable_with_elaboration(rt, declared, ll, Some(rl));
                    }
                    return rt;
                }
                if valid_target
                    && !self.store().is_error(target)
                    && target != TypeId::UNRESOLVED
                    && !self.is_commonjs_export_declaration(Loc::new(f, lnode))
                {
                    self.check_property_write(rt, target, lnode, ll, rl);
                }
                rt
            }
            AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign => {
                let lt = self.check_expr(ll);
                let rt = self.check_expr(rl);
                let result = match op {
                    AssignOp::AndAssign => {
                        let falsy = self.extract_definitely_falsy(lt);
                        self.union(&[falsy, rt])
                    }
                    AssignOp::OrAssign => {
                        let l2 = self.remove_definitely_falsy(lt);
                        self.union_with(&[l2, rt], Reduction::Subtype, AliasId::NONE)
                    }
                    _ => {
                        let l2 = self.non_nullable(lt);
                        self.union_with(&[l2, rt], Reduction::Subtype, AliasId::NONE)
                    }
                };
                if valid_target && !self.store().is_error(target) && target != TypeId::UNRESOLVED {
                    self.check_property_write(rt, target, lnode, ll, rl);
                }
                result
            }
            _ => {
                let lt = self.check_expr(ll);
                // An invalid target (`f += 1` for a function) is an error type: no
                // operator error.
                let lt = if valid_target { lt } else { TypeId::ANY };
                let rt = self.check_expr(rl);
                let opstr = match op {
                    AssignOp::AddAssign => "+",
                    AssignOp::SubAssign => "-",
                    AssignOp::MulAssign => "*",
                    AssignOp::DivAssign => "/",
                    AssignOp::ModAssign => "%",
                    AssignOp::ExpAssign => "**",
                    AssignOp::ShlAssign => "<<",
                    AssignOp::ShrAssign => ">>",
                    AssignOp::UShrAssign => ">>>",
                    AssignOp::BitOrAssign => "|",
                    AssignOp::BitXorAssign => "^",
                    _ => "&",
                };
                let shown = format!("{opstr}=");
                // An operand error (`x1 *= true` with `x1: boolean`) skips the write check.
                let mark = self.diags.len();
                let result = if op == AssignOp::AddAssign {
                    self.check_plus(e, &shown, ll, rl, lt, rt)
                } else {
                    self.check_arithmetic(e, opstr, &shown, ll, rl, lt, rt)
                };
                // The result must fit the left side's read (narrowed) type with literals
                // widened: `x: string | number` narrowed to `number` rejects `x += "a"`,
                // `k: 0 | 1` accepts `k += 1` (tsc 7).
                // A setter with its own type takes its declared type.
                let lnode_loc = Loc::new(f, lnode);
                let own_setter =
                    matches!(ast.node(lnode), Node::Member { .. } | Node::Index { .. })
                        && self
                            .property_symbol_of_access(lnode_loc)
                            .is_some_and(|p| self.setter_type(p).is_some());
                let write = if target == TypeId::UNRESOLVED
                    || self.store().is_error(target)
                    || own_setter
                {
                    target
                } else {
                    self.base_of_literal(lt)
                };
                // The target's shape is checked unless an operand type was rejected (a
                // null operand, TS18050, does not count).
                let operand_error = self
                    .diags
                    .get(mark..)
                    .is_some_and(|ds| ds.iter().any(|d| matches!(d.code, 2362 | 2363 | 2365)));
                let clean = self.diags.len() == mark;
                if valid_target
                    && !operand_error
                    && self.check_assignment_target_shape(ll)
                    && clean
                    && !self.store().is_error(write)
                    && write != TypeId::UNRESOLVED
                    && !self.store().is_error(result)
                {
                    self.check_type_related(
                        result,
                        write,
                        RelationKind::Assignable,
                        Some(ll),
                        None,
                    );
                }
                result
            }
        }
    }

    /// The type a write to property `p` must satisfy: the setter's parameter type for an
    /// accessor whose setter is annotated (`get x(): string; set x(v: string | number)`),
    /// else the property's type.
    pub(crate) fn write_type_of_symbol(&mut self, p: SymId) -> TypeId {
        let read = self.type_of_symbol(p);
        let (base, mapper) = match self.c.synth_sym(p) {
            Some(x)
                if x.origins.len() == 1
                    && x.mapped.is_none()
                    && x.check & check_flags::MERGED_MEMBER == 0 =>
            {
                (x.origins[0], x.mapper)
            }
            Some(_) => return read,
            None => (p, ts_r_types::MapperId::IDENTITY),
        };
        if self.c.synth_sym(base).is_some()
            || !self
                .c
                .sym_flags(base)
                .contains(SymbolFlags::GET_ACCESSOR | SymbolFlags::SET_ACCESSOR)
        {
            return read;
        }
        for d in self.c.decls(base) {
            let ast = self.ast(d.file);
            if !matches!(
                *ast.node(d.node),
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Set,
                    ..
                } | Node::MethodSignature {
                    kind: ts_r_ast::MethodKind::Set,
                    ..
                }
            ) {
                continue;
            }
            let Some(parts) = crate::nodes::fn_parts(ast, d.node) else {
                continue;
            };
            let param = ast
                .list(parts.params)
                .iter()
                .copied()
                .find(|&q| !crate::nodes::is_this_param(ast, q));
            if let Some(q) = param
                && let Node::Param {
                    type_ann: Some(ta), ..
                } = *ast.node(q)
            {
                let t = self.type_from_node(Loc::new(d.file, ta));
                return self.instantiate(t, mapper);
            }
        }
        read
    }

    /// The type a write through a union of literal keys must satisfy (`o[k] = v` with
    /// `k: "a" | "b"`): the intersection of each property's write type. `None` when some key
    /// names no property.
    fn union_key_write_type(&mut self, nt: TypeId, key: TypeId) -> Option<TypeId> {
        let ms = self.store().members_of_union(key).to_vec();
        let mut parts = Vec::with_capacity(ms.len());
        for m in ms {
            let name = self.property_name_of_type(m)?;
            let nid = self.store().intern_str(&name);
            let p = self.property_of_type_id(nt, nid)?;
            let w = match self.setter_type(p) {
                Some(w) => w,
                None => {
                    let wt = self.write_type_of_symbol(p);
                    self.write_type_of_property(wt)
                }
            };
            parts.push(w);
        }
        Some(self.intersection(&parts))
    }

    /// The type a write to a property of declared type `t` must satisfy: without the
    /// `missing` type of an optional property under `exactOptionalPropertyTypes`.
    pub(crate) fn write_type_of_property(&mut self, t: TypeId) -> TypeId {
        if self.c.opts.exact_optional_property_types {
            self.remove_missing(t)
        } else {
            t
        }
    }

    /// Whether `source` carries an `undefined` that `target` doesn't accept: the
    /// `exactOptionalPropertyTypes` mismatch (TS2412/TS2375 heads).
    pub(crate) fn is_exact_optional_mismatch(&mut self, source: TypeId, target: TypeId) -> bool {
        if !self.c.opts.exact_optional_property_types {
            return false;
        }
        let st = self.store();
        self.some_member(source, |m| st.flags(m).contains(TypeFlags::UNDEFINED))
            && !self.some_member(target, |m| {
                m != TypeId::MISSING
                    && st
                        .flags(m)
                        .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID)
            })
            && !st
                .flags(target)
                .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
    }

    /// Checks an assignment's value against its target type; a write to a property
    /// (`o.p = v`) whose value brings an `undefined` the target doesn't accept under
    /// `exactOptionalPropertyTypes` gets the TS2412 head.
    fn check_property_write(
        &mut self,
        rt: TypeId,
        target: TypeId,
        lnode: NodeId,
        ll: Loc,
        rl: Loc,
    ) {
        let ast = self.ast(ll.file);
        if !matches!(ast.node(lnode), Node::Member { .. })
            || !self.is_exact_optional_mismatch(rt, target)
        {
            self.check_assignable_with_elaboration(rt, target, ll, Some(rl));
            return;
        }
        if self.is_assignable(rt, target) || !self.reporting(ll.file) {
            return;
        }
        if self.elaborate_error(rl, rt, target) {
            return;
        }
        let head = Head {
            info: dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_WITH_EXACTOPTIONALPROPERTYTYPES_TRUE_CONSIDER_2412,
        };
        self.report_assignability(rt, target, ll, None, Some(head));
    }

    /// Checks `source` assignable to `target`, elaborating into object/array literals and
    /// arrow bodies at `expr` (tsc's `checkTypeAssignableToAndOptionallyElaborate`).
    pub fn check_assignable_with_elaboration(
        &mut self,
        source: TypeId,
        target: TypeId,
        node: Loc,
        expr: Option<Loc>,
    ) -> bool {
        if self.is_assignable(source, target) {
            return true;
        }
        if !self.reporting(node.file) {
            return false;
        }
        self.report_assignability(source, target, node, expr, None);
        false
    }

    /// Reports a failed assignability check (after the quiet check failed).
    pub fn report_assignability(
        &mut self,
        source: TypeId,
        target: TypeId,
        node: Loc,
        expr: Option<Loc>,
        head: Option<Head>,
    ) {
        if head.is_none()
            && let Some(x) = expr
            && self.elaborate_error(x, source, target)
        {
            return;
        }
        let (chain, at) = self.relation_error_chain(source, target, RelationKind::Assignable, head);
        let at = at.unwrap_or(node);
        self.error_chain(at, &chain);
    }

    /// "Did you mean to call this expression?" (or to use `new`) at `el`, for the diagnostic
    /// reported since `mark`, when calling `source` would give a `target`.
    pub(crate) fn relate_call_hint(
        &mut self,
        mark: usize,
        el: Loc,
        source: TypeId,
        target: TypeId,
    ) {
        if self.diags.len() <= mark || !self.would_fit_if_called(source, target) {
            return;
        }
        let info = if self
            .signatures_of(source, crate::members::SigKind::Call)
            .is_empty()
        {
            dg::DID_YOU_MEAN_TO_USE_NEW_WITH_THIS_EXPRESSION_6213
        } else {
            dg::DID_YOU_MEAN_TO_CALL_THIS_EXPRESSION_6212
        };
        self.relate_last(mark, el, info, &[]);
    }

    /// Whether `source` has call signatures (else construct signatures) and one of their
    /// return types is assignable to `target` (any parameters).
    fn would_fit_if_called(&mut self, source: TypeId, target: TypeId) -> bool {
        let sf = self.store().flags(source);
        if !sf.contains(TypeFlags::OBJECT) || sf.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            return false;
        }
        let mut sigs = self.signatures_of(source, crate::members::SigKind::Call);
        if sigs.is_empty() {
            sigs = self.signatures_of(source, crate::members::SigKind::Construct);
        }
        sigs.into_iter().any(|s| {
            let r = self.return_type_of(s);
            // (An `any` or `never` result is no hint.)
            r != TypeId::UNRESOLVED
                && !self
                    .store()
                    .flags(r)
                    .intersects(TypeFlags::ANY | TypeFlags::NEVER)
                && self.is_assignable(r, target)
        })
    }

    /// Reports errors inside an object/array literal or arrow body instead of on the whole
    /// expression; returns whether anything was reported.
    pub fn elaborate_error(&mut self, expr: Loc, source: TypeId, target: TypeId) -> bool {
        // A function (or class) whose call (or `new`) would fit is reported at the
        // expression itself ("Did you mean to call this expression?"); an object literal
        // method stays reported at its name.
        let ast = self.ast(expr.file);
        let el = Loc::new(expr.file, skip_parens(ast, expr.node));
        let method = matches!(ast.node(el.node), Node::MethodDef { .. })
            || self
                .c
                .parent(el)
                .is_some_and(|p| matches!(self.node(p), Node::MethodDef { .. }));
        if !method && self.would_fit_if_called(source, target) {
            let (chain, at) =
                self.relation_error_chain(source, target, RelationKind::Assignable, None);
            let mark = self.diags.len();
            self.error_chain(at.unwrap_or(el), &chain);
            self.relate_call_hint(mark, el, source, target);
            return true;
        }
        self.elaborate_literal_error(expr, source, target)
    }

    /// `elaborate_error` without the call hint (an argument is reported at itself anyway,
    /// with the argument message).
    pub fn elaborate_literal_error(&mut self, expr: Loc, source: TypeId, target: TypeId) -> bool {
        let ast = self.ast(expr.file);
        let n = skip_parens(ast, expr.node);
        let el = Loc::new(expr.file, n);
        let tf = self.store().flags(target);
        if tf.intersects(TypeFlags::PRIMITIVE) && !tf.contains(TypeFlags::UNION) {
            return false;
        }
        // An object literal against `X[] | X | undefined` is not elaborated: the member it is
        // matched with is the first one that is not array-like, and `null` and `undefined`
        // come first.
        if tf.contains(TypeFlags::UNION) && matches!(ast.node(n), Node::ObjectLit { .. }) {
            let ms = self.store().members_of_union(target).to_vec();
            if ms
                .iter()
                .any(|&m| m == TypeId::NULL || m == TypeId::UNDEFINED)
                && ms.iter().any(|&m| self.c.is_array_like_type(m))
                && self
                    .matching_discriminant_type(source, target, RelationKind::Assignable)
                    .is_none()
            {
                return false;
            }
        }
        // `T | undefined` (an optional parameter or property) elaborates against `T`.
        let target = if tf.contains(TypeFlags::UNION)
            && matches!(ast.node(n), Node::ObjectLit { .. } | Node::ArrayLit { .. })
        {
            let nn = self.remove_nullish(target);
            if nn != TypeId::NEVER && !self.store().flags(nn).intersects(TypeFlags::PRIMITIVE) {
                nn
            } else {
                target
            }
        } else {
            target
        };
        match *ast.node(n) {
            Node::ObjectLit { props } => {
                if self.store().flags(target).contains(TypeFlags::UNION) {
                    // A discriminated union elaborates against the selected members (their
                    // union when several).
                    if let Some(d) =
                        self.matching_discriminant_type(source, target, RelationKind::Assignable)
                    {
                        return self.elaborate_object(el, props, source, d, None);
                    }
                    // Otherwise each property against the union's own property, or the best
                    // matching member's; generic members (`T | X`) are not elaborated.
                    let generic = self
                        .store()
                        .members_of_union(target)
                        .iter()
                        .any(|&m| self.store().flags(m).intersects(TypeFlags::INSTANTIABLE));
                    if generic {
                        return false;
                    }
                    let best = self.best_matching_type(source, target, RelationKind::Assignable);
                    return self.elaborate_object(el, props, source, target, best);
                }
                self.elaborate_object(el, props, source, target, None)
            }
            Node::ArrayLit { elements } => {
                if self.store().flags(target).contains(TypeFlags::UNION) {
                    // A union with one array-like member elaborates against it (`X |
                    // X[]` reports the bad element).
                    let arrays: Vec<TypeId> = self
                        .store()
                        .members_of_union(target)
                        .to_vec()
                        .into_iter()
                        .filter(|&m| {
                            self.c.is_array_like_type(m)
                                || self.store().flags(m).contains(TypeFlags::OBJECT)
                                    && self.index_info_of(m, TypeId::NUMBER).is_some()
                        })
                        .collect();
                    return match arrays[..] {
                        [a] => self.elaborate_array(el, elements, a),
                        _ => false,
                    };
                }
                self.elaborate_array(el, elements, target)
            }
            Node::JsxOpening { .. } => {
                if self.store().flags(target).contains(TypeFlags::UNION) {
                    return false;
                }
                self.elaborate_jsx_attributes(el, source, target)
            }
            Node::Arrow {
                body,
                flags,
                params,
                ..
            } if flags.contains(ts_r_ast::FnFlags::EXPRESSION_BODY) => {
                // An arrow with an annotated parameter is not elaborated (observed:
                // `(x: string) => 1` reports the whole argument, `(x) => 1` the body).
                if ast.list(params).iter().any(|&p| {
                    matches!(
                        *ast.node(p),
                        Node::Param {
                            type_ann: Some(_),
                            ..
                        }
                    )
                }) {
                    return false;
                }
                // `() => expr` against a single target signature: elaborate the return.
                // A union of function types is elaborated only when every member is a
                // function with the same return type (observed tsc 7: `(() => number) |
                // ((a: string) => number)` is, `((n) => number) | ((n) => string)` and
                // `(() => number) | undefined` are not).
                if self.store().flags(target).contains(TypeFlags::UNION) {
                    let members = self.store().members_of_union(target).to_vec();
                    let mut ret = None;
                    for m in members {
                        let sigs = self.signatures_of(m, crate::members::SigKind::Call);
                        let [sig] = sigs[..] else {
                            return false;
                        };
                        let r = self.return_type_of(sig);
                        if ret.is_some_and(|x| x != r) {
                            return false;
                        }
                        ret = Some(r);
                    }
                }
                let tsigs = self.signatures_of(target, crate::members::SigKind::Call);
                let ssigs = self.signatures_of(source, crate::members::SigKind::Call);
                if tsigs.len() != 1 || ssigs.len() != 1 {
                    return false;
                }
                // The return is elaborated whenever it is not assignable, even when the
                // parameters are not related either (observed: `(x) => true`, its `x` fixed
                // to `number` by an earlier overload, against `(x: string) => string`
                // reports `true`).
                let tr = self.return_type_of(tsigs[0]);
                let sr = self.return_type_of(ssigs[0]);
                if self.is_assignable(sr, tr) {
                    return false;
                }
                let bl = Loc::new(expr.file, body);
                if self.elaborate_error(bl, sr, tr) {
                    return true;
                }
                let (chain, at) = self.relation_error_chain(sr, tr, RelationKind::Assignable, None);
                self.error_chain(at.unwrap_or(bl), &chain);
                true
            }
            _ => false,
        }
    }

    /// `fallback`: the union member to read a property from when the (union) target does
    /// not have it.
    fn elaborate_object(
        &mut self,
        el: Loc,
        props: ts_r_ast::NodeList,
        source: TypeId,
        target: TypeId,
        fallback: Option<TypeId>,
    ) -> bool {
        let ast = self.ast(el.file);
        let mut reported = false;
        let plain_source = !self
            .store()
            .flags(source)
            .intersects(TypeFlags::UNION | TypeFlags::INTERSECTION)
            && self.c.file(el.file).is_js()
            && self.c.parent(el).is_some_and(
                |p| matches!(*ast.node(p.node), Node::Assign { right, .. } if right == el.node),
            );
        for &m in ast.list(props) {
            let (key, value) = match *ast.node(m) {
                Node::Property { key, value, .. } => (key, value),
                Node::ShorthandProp { name, .. } => (name, name),
                Node::MethodDef {
                    key,
                    value,
                    kind: MethodKind::Method,
                    ..
                } => (key, value),
                _ => continue,
            };
            let Some(name) = key_text(ast, key).or_else(|| match *ast.node(key) {
                Node::ComputedKey { expr } => {
                    let kt = self.quietly(|ck| ck.check_expr(Loc::new(el.file, expr)));
                    self.property_name_of_type(self.store().regular(kt))
                }
                _ => None,
            }) else {
                continue;
            };
            let nid = self.store().intern_str(&name);
            let target = match fallback {
                Some(b) if self.property_of_type_id(target, nid).is_none() => b,
                _ => target,
            };
            let tprop_type = match self.property_of_type_id(target, nid) {
                Some(p) => {
                    // Optional target properties accept `undefined` from the source value.
                    self.type_of_symbol(p)
                }
                None => {
                    let numeric = is_numeric_name(&name);
                    let info = if numeric {
                        self.index_info_of(target, TypeId::NUMBER)
                    } else {
                        self.index_info_of(target, TypeId::STRING)
                    };
                    match info {
                        Some(i) => i.value,
                        None => continue,
                    }
                }
            };
            let vl = Loc::new(el.file, value);
            // An earlier property of a name written again is checked with the type the
            // last one gives the literal (reported at its name).
            let later_same = ast.list(props).iter().skip_while(|&&x| x != m).skip(1).any(|&x| {
                matches!(*ast.node(x), Node::Property { key: k, .. } if key_text(ast, k).as_deref() == Some(name.as_str()))
            });
            if later_same && let Some(sp) = self.property_of_type_id(source, nid) {
                let st = self.type_of_symbol(sp);
                if !self.is_assignable(st, tprop_type) {
                    let kl = Loc::new(el.file, key);
                    let (chain, _) =
                        self.relation_error_chain(st, tprop_type, RelationKind::Assignable, None);
                    let mark = self.diags.len();
                    self.error_chain(kl, &chain);
                    self.relate_expected_property(mark, target, nid, &name);
                    reported = true;
                }
                continue;
            }
            let vt = self.quietly(|ck| ck.check_expr(vl));
            // A nested object literal stays fresh: its excess properties are reported.
            let fresh_object = self
                .store()
                .object_of(vt)
                .is_some_and(|o| o.flags.contains(ts_r_types::ObjectFlags::FRESH_LITERAL));
            // (A literal value widens as in its object literal: `"true"` against
            // `boolean | undefined` is reported as `string`.)
            let vt = if fresh_object {
                vt
            } else {
                let w = if self.has_fresh_literal(vt)
                    && !self
                        .store()
                        .flags(vt)
                        .intersects(TypeFlags::BOOLEAN_LITERAL | TypeFlags::BOOLEAN)
                    && !self.in_const_context(vl)
                    && !self.is_literal_of_contextual_type(vt, tprop_type)
                {
                    self.widen_literal(vt)
                } else {
                    vt
                };
                self.regular_of(w)
            };
            let vt = if self.is_assignable(vt, tprop_type) {
                // A literal widened in the source (JavaScript `ns.x = { s: 'a' }`, not
                // contextually typed) is reported with the source's property type.
                match self.property_of_type_id(source, nid) {
                    Some(sp) if plain_source => {
                        let st = self.type_of_symbol(sp);
                        if self.is_assignable(st, tprop_type) {
                            continue;
                        }
                        st
                    }
                    _ => continue,
                }
            } else {
                vt
            };
            if self.elaborate_error(vl, vt, tprop_type) {
                reported = true;
                continue;
            }
            let kl = Loc::new(el.file, key);
            // An optional target property under `exactOptionalPropertyTypes` given a value
            // that may be `undefined`: the TS2412 message alone.
            if self.some_member(tprop_type, |m| m == TypeId::MISSING)
                && self.is_exact_optional_mismatch(vt, tprop_type)
            {
                let ss = self.type_to_string(vt);
                let ts = self.type_to_string(tprop_type);
                self.error(
                    kl,
                    dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_WITH_EXACTOPTIONALPROPERTYTYPES_TRUE_CONSIDER_2412,
                    &[&ss, &ts],
                );
                reported = true;
                continue;
            }
            let (chain, at) =
                self.relation_error_chain(vt, tprop_type, RelationKind::Assignable, None);
            let mark = self.diags.len();
            self.error_chain(at.unwrap_or(kl), &chain);
            self.relate_expected_property(mark, target, nid, &name);
            reported = true;
        }
        reported
    }

    /// "The expected type comes from property 'x' which is declared here on type 'T'" at the
    /// target property's declaration, for the diagnostic reported since `mark`.
    pub(crate) fn relate_expected_property(
        &mut self,
        mark: usize,
        target: TypeId,
        nid: ts_r_types::StrId,
        name: &str,
    ) {
        if self.diags.len() <= mark {
            return;
        }
        let Some(p) = self.property_of_type_id(target, nid) else {
            // From an index signature: "The expected type comes from this index signature."
            let name_text = self.store().str(nid).to_owned();
            let key = if is_numeric_name(&name_text) {
                TypeId::NUMBER
            } else {
                TypeId::STRING
            };
            let info = self
                .index_info_of(target, key)
                .or_else(|| self.index_info_of(target, TypeId::STRING));
            if let Some((f, n)) = info.and_then(|i| i.decl)
                && let Some(node) = NodeId::from_raw(n)
            {
                self.relate_last(
                    mark,
                    Loc::new(ts_r_diagnostics::FileId(f), node),
                    dg::THE_EXPECTED_TYPE_COMES_FROM_THIS_INDEX_SIGNATURE_6501,
                    &[],
                );
            }
            return;
        };
        let Some(d) = self.c.value_decl(p).or_else(|| self.c.first_decl(p)) else {
            return;
        };
        let ts = self.type_to_string(target);
        self.relate_last(
            mark,
            d,
            dg::THE_EXPECTED_TYPE_COMES_FROM_PROPERTY_0_WHICH_IS_DECLARED_HERE_ON_TYPE_1_6500,
            &[name, &ts],
        );
    }

    /// The type at position `i` of a tuple: the element, or past the first variable-length
    /// element the union of it and every element after it.
    fn tuple_position_type(&mut self, tuple: TypeId, i: usize) -> TypeId {
        let Some(shape) = self.store().tuple_shape_of(tuple).cloned() else {
            // A lone rest element is an array.
            return self
                .element_type_of_array_like(tuple)
                .unwrap_or(TypeId::UNRESOLVED);
        };
        let args = self.c.type_args_of_tuple(tuple).to_vec();
        let first_var = shape.elements.iter().position(|e| {
            e.kind == ts_r_types::TupleElement::REST || e.kind == ts_r_types::TupleElement::VARIADIC
        });
        match first_var {
            Some(v) if i >= v => {
                let mut parts = Vec::new();
                for (k, e) in shape.elements.iter().enumerate().skip(v) {
                    let a = args.get(k).copied().unwrap_or(TypeId::UNRESOLVED);
                    parts.push(if e.kind == ts_r_types::TupleElement::REST {
                        self.element_type_of_array_like(a).unwrap_or(a)
                    } else {
                        a
                    });
                }
                self.union(&parts)
            }
            _ => args.get(i).copied().unwrap_or(TypeId::UNRESOLVED),
        }
    }

    /// The tuple type of an array literal's elements, spreads expanded (fixed tuples) or
    /// kept as rest elements (arrays and iterables).
    fn tupleized_array_literal(
        &mut self,
        file: ts_r_diagnostics::FileId,
        elements: ts_r_ast::NodeList,
    ) -> TypeId {
        let ast = self.ast(file);
        let mut els: Vec<TupleEl> = Vec::new();
        for &x in ast.list(elements) {
            let l = Loc::new(file, x);
            let (ty, kind) = match *ast.node(x) {
                Node::Spread { arg } => {
                    let t = self.check_expr(Loc::new(file, arg));
                    if self.store().tuple_shape_of(t).is_some() {
                        (t, ts_r_types::TupleElement::VARIADIC)
                    } else {
                        let it = if self.store().flags(t).intersects(TypeFlags::ANY) {
                            t
                        } else {
                            self.iterated_type(t, false).unwrap_or(TypeId::UNRESOLVED)
                        };
                        (self.create_array(it, false), ts_r_types::TupleElement::REST)
                    }
                }
                _ => (
                    self.check_expr_mutable(l),
                    ts_r_types::TupleElement::REQUIRED,
                ),
            };
            els.push(TupleEl {
                ty,
                kind,
                label: None,
            });
        }
        self.create_tuple(&els, false)
    }

    fn elaborate_array(&mut self, el: Loc, elements: ts_r_ast::NodeList, target: TypeId) -> bool {
        let ast = self.ast(el.file);
        let mut reported = false;
        let is_tuple = self.store().tuple_shape_of(target).is_some();
        // With spreads, each position is first compared as the literal's tuple type has it
        // (`[...syms, 1]` has `number | symbol` at 1), then as the element itself.
        let spread_source = if ast
            .list(elements)
            .iter()
            .any(|&x| matches!(ast.node(x), Node::Spread { .. }))
        {
            if ast
                .list(elements)
                .iter()
                .any(|&x| matches!(ast.node(x), Node::Hole {}))
            {
                return false;
            }
            // A literal whose tuple type is just an array is not elaborated.
            let src = self.quietly(|ck| ck.tupleized_array_literal(el.file, elements));
            if self.store().tuple_shape_of(src).is_none() {
                return false;
            }
            Some(src)
        } else {
            None
        };
        for (i, &x) in ast.list(elements).iter().enumerate() {
            if matches!(ast.node(x), Node::Hole {}) {
                return reported;
            }
            let tt = if is_tuple {
                // Only the fixed elements before a rest are properties of a tuple.
                let leading = self.store().tuple_shape_of(target).map_or(0, |sh| {
                    sh.elements
                        .iter()
                        .position(|e| {
                            e.kind == ts_r_types::TupleElement::REST
                                || e.kind == ts_r_types::TupleElement::VARIADIC
                        })
                        .unwrap_or(sh.elements.len())
                });
                if i >= leading {
                    continue;
                }
                let name = i.to_string();
                match self.property_of_type(target, &name) {
                    Some(p) => self.type_of_symbol(p),
                    None => continue,
                }
            } else {
                match self.index_info_of(target, TypeId::NUMBER) {
                    Some(info) => info.value,
                    None => return reported,
                }
            };
            let xl = Loc::new(el.file, x);
            if let Some(src) = spread_source {
                let sp = self.tuple_position_type(src, i);
                if sp == TypeId::UNRESOLVED || self.is_assignable(sp, tt) {
                    continue;
                }
                if let Node::Spread { arg } = *ast.node(x) {
                    let at = self.quietly(|ck| ck.check_expr(Loc::new(el.file, arg)));
                    let it = if self.store().flags(at).intersects(TypeFlags::ANY) {
                        at
                    } else {
                        self.iterated_type(at, false).unwrap_or(TypeId::UNRESOLVED)
                    };
                    let shown = if it != TypeId::UNRESOLVED && !self.is_assignable(it, tt) {
                        it
                    } else {
                        sp
                    };
                    let (chain, at) =
                        self.relation_error_chain(shown, tt, RelationKind::Assignable, None);
                    self.error_chain(at.unwrap_or(xl), &chain);
                    reported = true;
                    continue;
                }
                let xt = self.quietly(|ck| ck.check_expr_mutable(xl));
                if self.is_assignable(xt, tt) {
                    let (chain, at) =
                        self.relation_error_chain(sp, tt, RelationKind::Assignable, None);
                    self.error_chain(at.unwrap_or(xl), &chain);
                    reported = true;
                    continue;
                }
            }
            // (An object or array literal element is typed in the target's context and
            // stays fresh: excess properties in it are errors.)
            let xt = if matches!(
                ast.node(skip_parens(ast, x)),
                Node::ObjectLit { .. } | Node::ArrayLit { .. }
            ) {
                self.quietly(|ck| ck.check_expr_with_context(xl, tt))
            } else if spread_source.is_some() {
                self.quietly(|ck| ck.check_expr_mutable(xl))
            } else {
                let xt = self.quietly(|ck| ck.check_expr(xl));
                self.regular_of(xt)
            };
            if self.is_assignable(xt, tt) {
                continue;
            }
            if self.elaborate_error(xl, xt, tt) {
                reported = true;
                continue;
            }
            let (chain, at) = self.relation_error_chain(xt, tt, RelationKind::Assignable, None);
            self.error_chain(at.unwrap_or(xl), &chain);
            reported = true;
        }
        reported
    }

    // ---------------------------------------------------------------------------------
    // Assertions
    // ---------------------------------------------------------------------------------

    fn check_assertion(&mut self, e: Loc, expr: NodeId, ty: NodeId) -> TypeId {
        let ast = self.ast(e.file);
        let xl = Loc::new(e.file, expr);
        if crate::nodes::is_keyword(ast, ty, KeywordTypeKind::Const) {
            let t = self.check_expr(xl);
            return self.regular_of_literal(t);
        }
        let target = self.type_from_node(Loc::new(e.file, ty));
        let t = self.check_expr(xl);
        if target == TypeId::UNRESOLVED
            || t == TypeId::UNRESOLVED
            || self.store().is_error(t)
            || !self.reporting(e.file)
        {
            return target;
        }
        let src = self.regular_of(t);
        let src = self.base_of_literal(src);
        let widened = self.widened_type(src);
        if !self.is_comparable(target, widened) && !self.is_comparable(src, target) {
            let (mut chain, at_node) = self.relation_error_chain(
                src,
                target,
                RelationKind::Comparable,
                Some(Head { info: dg::CONVERSION_OF_TYPE_0_TO_TYPE_1_MAY_BE_A_MISTAKE_BECAUSE_NEITHER_TYPE_SUFFICIENTL_2352 }),
            );
            // An excess property in an element of an array literal stands alone, at the
            // property (`<{ id: number }[]>[{ foo: "s" }]`).
            let mut at_node = at_node;
            if self.c.is_array_type(src) || self.c.is_tuple_type(src) {
                if let Some(i) = chain.items.iter().position(|(c, _)| {
                    *c == dg::OBJECT_LITERAL_MAY_ONLY_SPECIFY_KNOWN_PROPERTIES_AND_0_DOES_NOT_EXIST_IN_TYPE_1_2353.code
                        || *c == dg::OBJECT_LITERAL_MAY_ONLY_SPECIFY_KNOWN_PROPERTIES_BUT_0_DOES_NOT_EXIST_IN_TYPE_1_D_2561.code
                }) {
                    chain.items.drain(..i);
                    chain.nested.clear();
                } else {
                    at_node = None;
                }
            } else {
                at_node = None;
            }
            // A JSDoc cast (`/** @type {T} */ (x)`) is reported at `T` (tsc 7).
            let at = if ast.is_jsdoc(ty) {
                Loc::new(e.file, ty)
            } else {
                at_node.unwrap_or(e)
            };
            self.error_chain(at, &chain);
        }
        target
    }
}

/// Identifiers and property access chains of identifiers (and `this`).
pub fn is_entity_name_expression(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    match *ast.node(n) {
        Node::Ident { .. } | Node::This {} => true,
        Node::Member {
            obj,
            prop,
            optional,
        } => {
            !optional
                && matches!(ast.node(prop), Node::Ident { .. })
                && is_entity_name_expression(ast, obj)
        }
        _ => false,
    }
}

/// Whether a property name is a numeric literal name (`"0"`, `"1.5"`).
pub fn is_numeric_name(name: &str) -> bool {
    match name.parse::<f64>() {
        Ok(v) => ts_r_binder::js_number_to_string(v) == name,
        Err(_) => false,
    }
}

impl Ck<'_, '_> {
    /// `typeof globalThis`: an object with the global `var`s, functions, classes, enums and
    /// namespaces as properties (not `let`/`const`), and `globalThis` itself.
    pub fn global_this_type(&mut self) -> TypeId {
        if let Some(t) = self.global_this {
            return t;
        }
        let names: Vec<Box<str>> = self.c.link.globals.iter().map(|(n, _)| n.clone()).collect();
        let mut props: Vec<(ts_r_types::StrId, ts_r_types::SymId)> = Vec::new();
        for n in names {
            let Some(s) = self.c.global_symbol(&n, meaning::VALUE) else {
                continue;
            };
            let f = self.c.sym_flags(s);
            if f.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE) || !f.intersects(SymbolFlags::VALUE)
            {
                continue;
            }
            props.push((self.store().intern_str(&n), s));
        }
        let st = self.store();
        let object = |props: Vec<(ts_r_types::StrId, ts_r_types::SymId)>| {
            st.literal_object(
                ts_r_types::SymId::NONE,
                ts_r_types::ObjectFlags::ANONYMOUS,
                ts_r_types::ResolvedMembers::new(props, Vec::new(), Vec::new(), Vec::new()),
                ts_r_types::TypeTraits::NONE,
            )
        };
        // The readonly `globalThis` property is the object itself. Its symbol is created
        // before its type, in the level of the other members (`without` places it there).
        let name = st.intern_str("globalThis");
        let without = object(props.clone());
        let me = self.c.new_synth(
            SynthSym::new(name, SymbolFlags::PROPERTY, check_flags::READONLY)
                .with_parent(Some(without)),
        );
        props.push((name, me));
        let t = object(props);
        if let Some(s) = self.c.synth_sym(me) {
            let _ = s.ty.set(t);
        }
        self.global_this = Some(t);
        t
    }
}

/// The "do you need to install type definitions" or "change your target library" variant
/// of TS2304 for a well-known global name, with the lib to suggest (an empty argument for
/// the variants that take none). tsc 7 (whose `types` default is empty) always uses the
/// "add it to the types field" variants; the name lists are observed.
pub(crate) fn missing_types_hint(name: &str) -> Option<(dg::DiagnosticInfo, &'static str)> {
    let lib = match name {
        "Map" | "Set" | "WeakMap" | "WeakSet" | "Reflect" | "Iterator" | "AsyncIterator" => {
            Some("es2015")
        }
        "Atomics" | "SharedArrayBuffer" => Some("es2017"),
        "AsyncGenerator" | "AsyncGeneratorFunction" | "AsyncIterable" | "AsyncIterableIterator" => {
            Some("es2018")
        }
        "BigInt" | "BigInt64Array" | "BigUint64Array" => Some("es2020"),
        _ => None,
    };
    if let Some(lib) = lib {
        return Some((
            dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_CHANGE_YOUR_TARGET_LIBRARY_TRY_CHANGING_THE_LI_2583,
            lib,
        ));
    }
    let info = match name {
        "require" | "process" | "Buffer" | "module" => {
            dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_INSTALL_TYPE_DEFINITIONS_FOR_NODE_TRY_NPM_I_SA_2591
        }
        "$" => dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_INSTALL_TYPE_DEFINITIONS_FOR_JQUERY_TRY_NPM_I_S_2592,
        "describe" | "suite" | "it" | "test" | "beforeEach" => {
            dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_INSTALL_TYPE_DEFINITIONS_FOR_A_TEST_RUNNER_TRY_2593
        }
        "Bun" => dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_INSTALL_TYPE_DEFINITIONS_FOR_BUN_TRY_NPM_I_SAV_2868,
        "document" | "console" => {
            dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_CHANGE_YOUR_TARGET_LIBRARY_TRY_CHANGING_THE_LI_2584
        }
        _ => return None,
    };
    Some((info, ""))
}

/// The index signature kind a non-literal computed key of an object literal contributes to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IndexKeyKind {
    String,
    Number,
    Symbol,
}

/// The `exports` identifier (the JavaScript CommonJS export object, by syntax).
fn is_exports_identifier(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    crate::nodes::ident_text(ast, n) == Some("exports")
}
