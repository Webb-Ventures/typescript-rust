//! Decorator placement (TS1206, TS1249).
//!
//! Rules observed from tsc 7.0.2 (reported once per decorated node, at its first `@`):
//! - Decorators on anything but a class or class member (`var`, `function`, `enum`,
//!   `interface`, `type`, `namespace`, imports and exports other than classes, static
//!   blocks): TS1206.
//! - Constructors: TS1206. Methods without a body (overloads, abstract and ambient
//!   methods): TS1249.
//! - Standard decorators (no `experimentalDecorators`): `abstract` and `declare` fields
//!   (including `abstract accessor`) and every parameter decorator: TS1206.
//! - `experimentalDecorators`: class expressions and members with `#private` names: TS1206;
//!   parameter decorators are valid only on parameters of class constructors, methods and
//!   accessors that have a body: TS1206 elsewhere.

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId, NodeList};
use ts_r_diagnostics::FileId;
use ts_r_span::Span;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::fn_parts;
use ts_r_types::TypeId;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the decorators written on node `n` of `file`.
    pub(crate) fn check_decorator_placement(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let exp = self.c.opts.experimental_decorators;
        match *ast.node(n) {
            // Decorators the parser accepted and dropped: the node starts at the `@`.
            Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::Enum { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Module { .. }
            | Node::ImportDecl { .. }
            | Node::ImportEquals { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportAssignment { .. }
            | Node::StaticBlock { .. } => {
                if self.c.text(Loc::new(file, n)).starts_with('@') {
                    let s = ast.span(n).start;
                    self.decorator_error(file, s, dg::DECORATORS_ARE_NOT_VALID_HERE_1206);
                }
            }
            Node::ClassDecl {
                decorators,
                modifiers,
                ..
            } => {
                // `@a export @b class C {}`: decorators on both sides of `export` (at the
                // first one after it).
                let list = ast.list(decorators);
                let text = &self.c.file(file).text;
                for w in list.windows(2) {
                    let gap = text
                        .get(ast.span(w[0]).end as usize..ast.span(w[1]).start as usize)
                        .unwrap_or("");
                    if modifiers.contains(Modifiers::EXPORT) && gap_has_export_keyword(gap) {
                        let s = ast.span(w[1]).start;
                        self.decorator_error(
                            file,
                            s,
                            dg::DECORATORS_MAY_NOT_APPEAR_AFTER_EXPORT_OR_EXPORT_DEFAULT_IF_THEY_ALSO_APPEAR_BEF_8038,
                        );
                        break;
                    }
                }
                // `export @dec default class C {}`: decorators between `export` and `default`.
                if let Some(&last) = ast.list(decorators).last() {
                    let text = &self.c.file(file).text;
                    let rest = text.get(ast.span(last).end as usize..).unwrap_or("");
                    let rest = rest.trim_start();
                    if modifiers.contains(Modifiers::EXPORT)
                        && rest.starts_with("default")
                        && rest[7..].starts_with(|c: char| c.is_ascii_whitespace())
                    {
                        self.first_decorator_error(
                            file,
                            decorators,
                            dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                        );
                    }
                }
            }
            Node::ClassExpr { decorators, .. } if exp => {
                self.first_decorator_error(
                    file,
                    decorators,
                    dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                );
            }
            Node::MethodDef {
                decorators,
                modifiers,
                kind,
                key,
                value,
                ..
            } => {
                if ast.list(decorators).is_empty() {
                    return;
                }
                if kind == MethodKind::Constructor
                    || (exp
                        && (self.in_class_expr(file, n)
                            || matches!(ast.node(key), Node::PrivateName { .. })))
                {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                    );
                } else if matches!(kind, MethodKind::Get | MethodKind::Set) {
                    // Accessors without a body: abstract ones are invalid under standard
                    // decorators; other cases are not modelled.
                    if !exp
                        && modifiers.contains(Modifiers::ABSTRACT)
                        && fn_parts(ast, value).is_some_and(|p| p.body.is_none())
                    {
                        self.first_decorator_error(
                            file,
                            decorators,
                            dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                        );
                    }
                } else if fn_parts(ast, value).is_some_and(|p| p.body.is_none()) {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::A_DECORATOR_CAN_ONLY_DECORATE_A_METHOD_IMPLEMENTATION_NOT_AN_OVERLOAD_1249,
                    );
                }
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                ..
            } => {
                if ast.list(decorators).is_empty() {
                    return;
                }
                let bad = if exp {
                    matches!(ast.node(key), Node::PrivateName { .. }) || self.in_class_expr(file, n)
                } else {
                    modifiers.intersects(Modifiers::ABSTRACT | Modifiers::DECLARE)
                };
                if bad {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                    );
                }
            }
            Node::Param { decorators, .. } => {
                let list = ast.list(decorators);
                let (Some(&first), Some(&last)) = (list.first(), list.last()) else {
                    return;
                };
                if crate::nodes::is_this_param(ast, n) {
                    // tsc's span starts after the previous token (leading trivia included).
                    let text = self.c.file(file).text.as_bytes();
                    let mut start = ast.span(first).start as usize;
                    while start > 0 && text.get(start - 1).is_some_and(|b| b.is_ascii_whitespace())
                    {
                        start -= 1;
                    }
                    let span = Span::new(start as u32, ast.span(last).end);
                    self.error_span(
                        file,
                        span,
                        dg::NEITHER_DECORATORS_NOR_MODIFIERS_MAY_BE_APPLIED_TO_THIS_PARAMETERS_1433,
                        &[],
                    );
                    return;
                }
                if !exp || !self.param_decorators_allowed(Loc::new(file, n)) {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                    );
                }
            }
            _ => {}
        }
    }

    /// Checks the expressions of the decorators written on class, member or parameter `n`
    /// (`@dec(C)` reads `C` like any expression: TS2304, TS2449, ...).
    pub(crate) fn check_decorator_expressions(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let decorators = match *ast.node(n) {
            // Invalid there (TS1206): tsc does not check the expressions.
            Node::ClassExpr { .. } if self.c.opts.experimental_decorators => return,
            Node::Param { .. }
                if crate::nodes::is_this_param(ast, n)
                    || !self.c.opts.experimental_decorators
                    || !self.param_decorators_allowed(Loc::new(file, n)) =>
            {
                return;
            }
            Node::ClassDecl { decorators, .. }
            | Node::ClassExpr { decorators, .. }
            | Node::PropertyDef { decorators, .. }
            | Node::MethodDef { decorators, .. }
            | Node::Param { decorators, .. } => decorators,
            _ => return,
        };
        for &d in ast.list(decorators) {
            if let Node::Decorator { expr } = *ast.node(d) {
                // A function decorator is contextually typed by the decorator signature;
                // its parameters are not implicitly `any` (modelled as an `any` context).
                let inner = crate::nodes::skip_parens(ast, expr);
                // `@await` in a module is an `await` with no operand (a parse error), not
                // a name.
                let head = match *ast.node(inner) {
                    Node::Call { callee, .. } => crate::nodes::skip_parens(ast, callee),
                    _ => inner,
                };
                if matches!(ast.node(head), Node::Ident { .. })
                    && self.c.text(Loc::new(file, head)) == "await"
                {
                    continue;
                }
                let t = if matches!(
                    ast.node(inner),
                    Node::FunctionExpr { .. } | Node::Arrow { .. }
                ) {
                    self.check_expr_with_context(Loc::new(file, inner), TypeId::ANY)
                } else {
                    self.check_expr(Loc::new(file, expr))
                };
                self.check_decorator_call(file, d, n, t);
            }
        }
        if !ast.list(decorators).is_empty() {
            self.check_decorator_metadata_imports(file, n);
        }
    }

    /// Under `isolatedModules` with `emitDecoratorMetadata` and ES module emit, the types
    /// a decorated node's metadata names (a class's constructor parameters, a method's
    /// parameters and return type, an accessor's or property's type, a decorated
    /// parameter's whole signature) must not be plain imports of types: the emitted
    /// metadata reads them as values. TS1272 at the type name (tsc: checker.go
    /// markDecoratorAliasReferenced). `verbatimModuleSyntax` turns the check off.
    fn check_decorator_metadata_imports(&mut self, file: FileId, n: NodeId) {
        use ts_r_config::ModuleKind as M;
        let o = &self.c.opts;
        if !o.emit_decorator_metadata
            || !o.isolated_modules
            || o.verbatim_module_syntax
            || matches!(o.module, M::CommonJS | M::AMD | M::UMD | M::System)
        {
            return;
        }
        let ast = self.ast(file);
        if !matches!(ast.node(n), Node::PropertyDef { .. })
            && self.is_ambient_node(Loc::new(file, n))
        {
            return;
        }
        let mut types: Vec<Option<NodeId>> = Vec::new();
        let sig_types = |f: NodeId, types: &mut Vec<Option<NodeId>>, ret: bool| {
            if let Node::FunctionExpr {
                params,
                return_type,
                ..
            } = *ast.node(f)
            {
                types.extend(
                    ast.list(params)
                        .iter()
                        .map(|&p| param_metadata_type(ast, p)),
                );
                if ret {
                    types.push(return_type);
                }
            }
        };
        match *ast.node(n) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                let ctor = ast.list(members).iter().find_map(|&m| match *ast.node(m) {
                    Node::MethodDef {
                        kind: MethodKind::Constructor,
                        value,
                        ..
                    } if matches!(ast.node(value), Node::FunctionExpr { body: Some(_), .. }) => {
                        Some(value)
                    }
                    _ => None,
                });
                if let Some(f) = ctor {
                    sig_types(f, &mut types, false);
                }
            }
            Node::MethodDef {
                kind: kind @ (MethodKind::Get | MethodKind::Set),
                key,
                modifiers,
                value,
                ..
            } => {
                let mut t = accessor_annotation(ast, kind, value);
                if t.is_none()
                    && let Some(class) = self.c.parent(Loc::new(file, n))
                    && let Some(members) = match *ast.node(class.node) {
                        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                            Some(members)
                        }
                        _ => None,
                    }
                {
                    let name = crate::nodes::key_text(ast, key);
                    let other_kind = if kind == MethodKind::Get {
                        MethodKind::Set
                    } else {
                        MethodKind::Get
                    };
                    let is_static = modifiers.contains(Modifiers::STATIC);
                    t = ast.list(members).iter().find_map(|&m| match *ast.node(m) {
                        Node::MethodDef {
                            kind: k,
                            key: k2,
                            modifiers: m2,
                            value: v2,
                            ..
                        } if k == other_kind
                            && m2.contains(Modifiers::STATIC) == is_static
                            && name.is_some()
                            && crate::nodes::key_text(ast, k2) == name =>
                        {
                            accessor_annotation(ast, k, v2)
                        }
                        _ => None,
                    });
                }
                types.push(t);
            }
            Node::MethodDef { value, .. } => sig_types(value, &mut types, true),
            Node::PropertyDef { type_ann, .. } => types.push(type_ann),
            Node::Param { .. } => {
                types.push(param_metadata_type(ast, n));
                if let Some(f) = self.c.parent(Loc::new(file, n)) {
                    sig_types(f.node, &mut types, true);
                }
            }
            _ => {}
        }
        let strict_null = self.c.opts.strict_null_checks;
        for t in types.into_iter().flatten() {
            let Some(name) = metadata_entity_name(ast, t, strict_null, 0) else {
                continue;
            };
            let mut root = name;
            let mut guard = 0;
            while let Node::QualifiedName { left, .. } = *ast.node(root) {
                root = left;
                guard += 1;
                if guard > 10_000 {
                    break;
                }
            }
            let m = if root == name {
                ts_r_binder::meaning::TYPE
            } else {
                ts_r_binder::meaning::NAMESPACE
            };
            let Some(s) = self.c.resolve_ident(Loc::new(file, root), m) else {
                continue;
            };
            if !self
                .c
                .sym_flags(s)
                .intersects(ts_r_binder::SymbolFlags::ALIAS)
                || self
                    .c
                    .sym_facts(s)
                    .intersects(ts_r_binder::SymbolFacts::TYPE_ONLY)
            {
                continue;
            }
            if self.c.alias_lacks_meaning(s, ts_r_binder::meaning::VALUE)
                || self.type_only_alias_file(s).is_some()
            {
                self.error(
                    Loc::new(file, name),
                    dg::A_TYPE_REFERENCED_IN_A_DECORATED_SIGNATURE_MUST_BE_IMPORTED_WITH_IMPORT_TYPE_OR_A_1272,
                    &[],
                );
            }
        }
    }

    /// `experimentalDecorators`: parameter `p` belongs to a class constructor, method or
    /// accessor with a body. Unknown owners count as allowed (no report).
    fn param_decorators_allowed(&self, p: Loc) -> bool {
        let Some(f) = self.c.parent(p) else {
            return true;
        };
        let ast = self.ast(p.file);
        match *ast.node(f.node) {
            Node::FunctionDecl { .. } => false,
            Node::FunctionExpr { .. } => {
                let Some(m) = self.c.parent(f) else {
                    return true;
                };
                if !matches!(ast.node(m.node), Node::MethodDef { .. }) {
                    return true;
                }
                // Class members only (object literal methods are left alone).
                let in_class = self.c.parent(m).is_some_and(|c| {
                    matches!(
                        ast.node(c.node),
                        Node::ClassDecl { .. } | Node::ClassExpr { .. }
                    )
                });
                !in_class || fn_parts(ast, f.node).is_none_or(|x| x.body.is_some())
            }
            _ => true,
        }
    }

    /// Whether class member `m` belongs to a class expression.
    fn in_class_expr(&self, file: FileId, m: NodeId) -> bool {
        self.c
            .parent(Loc::new(file, m))
            .is_some_and(|c| matches!(self.node(c), Node::ClassExpr { .. }))
    }

    fn first_decorator_error(
        &mut self,
        file: FileId,
        decorators: NodeList,
        info: dg::DiagnosticInfo,
    ) {
        let ast = self.ast(file);
        if let Some(&d) = ast.list(decorators).first() {
            let s = ast.span(d).start;
            self.decorator_error(file, s, info);
        }
    }

    fn decorator_error(&mut self, file: FileId, start: u32, info: dg::DiagnosticInfo) {
        self.error_span(file, Span::new(start, start + 1), info, &[]);
    }
}

/// The annotated type of an accessor: a getter's return type, a setter's parameter type.
fn accessor_annotation(ast: &ts_r_ast::Ast, kind: MethodKind, f: NodeId) -> Option<NodeId> {
    let Node::FunctionExpr {
        params,
        return_type,
        ..
    } = *ast.node(f)
    else {
        return None;
    };
    match kind {
        MethodKind::Get => return_type,
        _ => match ast.list(params).first().map(|&p| ast.node(p)) {
            Some(Node::Param { type_ann, .. }) => *type_ann,
            _ => None,
        },
    }
}

/// The type of parameter `p` its metadata describes: the element type of a rest
/// parameter's array type.
fn param_metadata_type(ast: &ts_r_ast::Ast, p: NodeId) -> Option<NodeId> {
    let Node::Param { rest, type_ann, .. } = *ast.node(p) else {
        return None;
    };
    let t = type_ann?;
    if !rest {
        return Some(t);
    }
    match *ast.node(t) {
        Node::ArrayType { elem } => Some(elem),
        Node::TypeRef {
            type_args: Some(a), ..
        } => match *ast.node(a) {
            Node::TypeArgs { args } => ast.list(args).first().copied(),
            _ => None,
        },
        _ => None,
    }
}

/// The entity name decorator metadata serializes type node `t` as: a type reference's
/// name, or the one name every member of a union or intersection (or both branches of a
/// conditional) shares, skipping `never` and, without `strictNullChecks`, `null` and
/// `undefined`.
fn metadata_entity_name(
    ast: &ts_r_ast::Ast,
    t: NodeId,
    strict_null: bool,
    depth: u32,
) -> Option<NodeId> {
    if depth > 64 {
        return None;
    }
    let from_list = |types: &[NodeId]| -> Option<NodeId> {
        let mut common: Option<NodeId> = None;
        for &m in types {
            match *ast.node(m) {
                Node::KeywordType {
                    kind: ts_r_ast::KeywordTypeKind::Never,
                } => continue,
                Node::KeywordType {
                    kind: ts_r_ast::KeywordTypeKind::Null | ts_r_ast::KeywordTypeKind::Undefined,
                } if !strict_null => continue,
                _ => {}
            }
            let one = metadata_entity_name(ast, m, strict_null, depth + 1)?;
            match common {
                None => common = Some(one),
                Some(c) => {
                    let same = matches!(
                        (ast.node(c), ast.node(one)),
                        (Node::Ident { name: a }, Node::Ident { name: b }) if a == b
                    );
                    if !same {
                        return None;
                    }
                }
            }
        }
        common
    };
    match *ast.node(t) {
        Node::UnionType { types } | Node::IntersectionType { types } => from_list(ast.list(types)),
        Node::ConditionalType {
            true_ty, false_ty, ..
        } => from_list(&[true_ty, false_ty]),
        Node::ParenType { ty } | Node::NamedTupleMember { ty, .. } => {
            metadata_entity_name(ast, ty, strict_null, depth + 1)
        }
        Node::TypeRef { name, .. } => Some(name),
        _ => None,
    }
}

/// Whether the text between two decorators holds the `export` keyword (comments skipped).
fn gap_has_export_keyword(gap: &str) -> bool {
    let b = gap.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            while i < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i += 2;
        } else if b[i..].starts_with(b"export")
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'$'))
            && b.get(i + 6)
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$'))
        {
            return true;
        } else {
            i += 1;
        }
    }
    false
}
