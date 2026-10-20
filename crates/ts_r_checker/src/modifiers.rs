//! Modifier grammar: one error per declaration (TS1024, TS1028-TS1031, TS1038, TS1040,
//! TS1042, TS1044, TS1046, TS1070, TS1071, TS1089, TS1090, TS1184, TS1242-TS1244, TS1253,
//! TS1275, TS1276, TS18010, TS18019) and TS1273/TS1274/TS1277 for type-parameter modifiers.
//!
//! Rules observed from tsc 7.0.2. The modifier keywords are visited in written order
//! (`Ast::modifier_tokens_of`) and the first one that breaks a rule is reported, at that
//! keyword; then a few whole-declaration checks run (`async` on a non-function: TS1042, or
//! TS1089 on a constructor; `accessor` with `?`: TS1276). For each keyword:
//! - Interface and type-literal members take only `readonly` (else TS1070; on a method
//!   signature TS1024); their index signatures too (else TS1071). Class index signatures
//!   take `static` and `readonly` (else TS1071). Modifiers before `static {}`: TS1184.
//! - A second accessibility modifier: TS1028. Any other repeat: TS1030.
//! - Order (TS1029, naming the first modifier already seen in this priority list):
//!   accessibility before override, static, accessor, readonly, async, abstract;
//!   `static` before readonly, async, accessor, override; `override` before readonly,
//!   accessor, async; `abstract` before override, accessor; `export` before declare, async,
//!   abstract. `abstract private` is TS1243 instead.
//! - Declarations in a block (not a file or namespace body): TS1184 for any modifier but
//!   `async` on a function and `abstract` on a class.
//! - Accessibility and `static` on a statement: TS1044. Accessibility on a `#private`
//!   member: TS18010; `declare` or `abstract` there: TS18019.
//! - `static`, `export`, `declare`, `async` on a parameter: TS1090; `abstract`: TS1242;
//!   `accessor`: TS1275. `static`, `override`, `async` on a constructor: TS1089.
//! - `export` on any class member, `declare` on methods, accessors and constructors: TS1031.
//! - `declare` after `async` or `override`: TS1040 naming that one; `override` after
//!   `declare`: TS1243. `declare` on a statement inside an ambient namespace, module or
//!   `declare global` (or any namespace of a `.d.ts` file): TS1038.
//! - `async` in an ambient context (a `.d.ts` file, a declare class or namespace, or after
//!   `declare`): TS1040.
//! - `readonly` only on class properties, index signatures, property signatures and
//!   parameters (else TS1024); with `accessor`: TS1243 at the later one, as for `declare`
//!   with `accessor`.
//! - `abstract` on a constructor, a parameter or a statement other than a class: TS1242;
//!   on a member of a class that is not abstract: TS1244 (methods, accessors) or TS1253
//!   (properties), before any other rule; with `static`, `private` or `async`: TS1243 at
//!   the later keyword (always at `async`).
//! - `accessor` only on class properties: TS1275.
//! - After the keyword rules: `declare` on an import declaration or `import x =` is TS1079
//!   at `declare`; then `export` on an import declaration is TS1191 at `export`.
//! - The first top-level `var`, function, class, enum or namespace of a `.d.ts` file
//!   without `declare` or `export`: TS1046 (once per file).
//!
//! Object-literal members are checked in `check.rs` (TS1042 on every modifier).

use ts_r_ast::{Ast, MethodKind, Modifiers, Node, NodeId, NodeKind, Span};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

const ACCESS: Modifiers = Modifiers::PUBLIC
    .union(Modifiers::PRIVATE)
    .union(Modifiers::PROTECTED);

/// The keyword of a single modifier flag.
pub(crate) fn modifier_text(m: Modifiers) -> &'static str {
    match m {
        Modifiers::EXPORT => "export",
        Modifiers::DEFAULT => "default",
        Modifiers::DECLARE => "declare",
        Modifiers::ABSTRACT => "abstract",
        Modifiers::PUBLIC => "public",
        Modifiers::PRIVATE => "private",
        Modifiers::PROTECTED => "protected",
        Modifiers::READONLY => "readonly",
        Modifiers::STATIC => "static",
        Modifiers::OVERRIDE => "override",
        Modifiers::ACCESSOR => "accessor",
        Modifiers::CONST => "const",
        Modifiers::IN => "in",
        Modifiers::OUT => "out",
        Modifiers::ASYNC => "async",
        _ => "",
    }
}

/// The first of `order` already in `seen`.
fn first_seen(seen: Modifiers, order: &[Modifiers]) -> Option<Modifiers> {
    order.iter().copied().find(|&m| seen.contains(m))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Top,
    Namespace,
    Block,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Property,
    Method,
    Accessor,
    Constructor,
}

/// Where a declaration with modifiers sits.
#[derive(Clone, Copy)]
enum Site {
    Member {
        kind: Member,
        private: bool,
        ambient: bool,
        abstract_class: bool,
    },
    ClassIndex,
    TypeIndex,
    TypeMember {
        method: bool,
    },
    Param,
    StaticBlock,
    Stmt {
        kind: NodeKind,
        place: Place,
        ambient: bool,
    },
}

/// A modifier error: where, which message, and its arguments (empty ones unused).
pub(crate) type ModifierError = (Span, dg::DiagnosticInfo, [&'static str; 2]);

fn err(span: Span, info: dg::DiagnosticInfo, a: &'static str, b: &'static str) -> ModifierError {
    (span, info, [a, b])
}

/// The rules for keyword `f` at `span`, with the modifiers in `seen` written before it.
/// `async_span` is where an earlier `async` was written.
fn keyword_error(
    site: Site,
    f: Modifiers,
    span: Span,
    seen: Modifiers,
    async_span: Option<Span>,
) -> Option<ModifierError> {
    let w = modifier_text(f);
    let here = |info| Some(err(span, info, "", ""));
    let named = |info| Some(err(span, info, w, ""));
    match site {
        Site::TypeMember { .. } if f != Modifiers::READONLY => {
            return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_TYPE_MEMBER_1070);
        }
        Site::TypeIndex if f != Modifiers::READONLY => {
            return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_AN_INDEX_SIGNATURE_1071);
        }
        Site::ClassIndex if !(f == Modifiers::STATIC || f == Modifiers::READONLY) => {
            return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_AN_INDEX_SIGNATURE_1071);
        }
        Site::StaticBlock => return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184),
        _ => {}
    }
    let block = matches!(
        site,
        Site::Stmt {
            place: Place::Block,
            ..
        }
    );
    let stmt_top = matches!(
        site,
        Site::Stmt {
            place: Place::Top | Place::Namespace,
            ..
        }
    );
    let member = |k: Member| matches!(site, Site::Member { kind, .. } if kind == k);
    let is_member = matches!(site, Site::Member { .. });
    let private_name = matches!(site, Site::Member { private: true, .. });
    let param = matches!(site, Site::Param);
    let order = |list: &[Modifiers]| {
        first_seen(seen, list).map(|m| {
            err(
                span,
                dg::_0_MODIFIER_MUST_PRECEDE_1_MODIFIER_1029,
                w,
                modifier_text(m),
            )
        })
    };
    let with = |at: Span, a: &'static str, b: &'static str| {
        Some(err(
            at,
            dg::_0_MODIFIER_CANNOT_BE_USED_WITH_1_MODIFIER_1243,
            a,
            b,
        ))
    };
    if f.intersects(ACCESS) {
        if seen.intersects(ACCESS) {
            return here(dg::ACCESSIBILITY_MODIFIER_ALREADY_SEEN_1028);
        }
        const AFTER: [Modifiers; 6] = [
            Modifiers::OVERRIDE,
            Modifiers::STATIC,
            Modifiers::ACCESSOR,
            Modifiers::READONLY,
            Modifiers::ASYNC,
            Modifiers::ABSTRACT,
        ];
        // `abstract private` is TS1243, not an order error.
        let list = if f == Modifiers::PRIVATE {
            &AFTER[..5]
        } else {
            &AFTER[..]
        };
        if let Some(e) = order(list) {
            return Some(e);
        }
        if f == Modifiers::PRIVATE && is_member && seen.contains(Modifiers::ABSTRACT) {
            return with(span, "private", "abstract");
        }
        if stmt_top {
            return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_MODULE_OR_NAMESPACE_ELEMENT_1044);
        }
        if block {
            return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
        }
        if private_name {
            return here(
                dg::AN_ACCESSIBILITY_MODIFIER_CANNOT_BE_USED_WITH_A_PRIVATE_IDENTIFIER_18010,
            );
        }
        return None;
    }
    if seen.contains(f) {
        return named(dg::_0_MODIFIER_ALREADY_SEEN_1030);
    }
    match f {
        Modifiers::STATIC => {
            if let Some(e) = order(&[
                Modifiers::READONLY,
                Modifiers::ASYNC,
                Modifiers::ACCESSOR,
                Modifiers::OVERRIDE,
            ]) {
                return Some(e);
            }
            if stmt_top {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_MODULE_OR_NAMESPACE_ELEMENT_1044);
            }
            if block {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
            if param {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_PARAMETER_1090);
            }
            if member(Member::Constructor) {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_CONSTRUCTOR_DECLARATION_1089);
            }
            if is_member && seen.contains(Modifiers::ABSTRACT) {
                return with(span, "static", "abstract");
            }
        }
        Modifiers::EXPORT => {
            if let Some(e) = order(&[Modifiers::DECLARE, Modifiers::ASYNC, Modifiers::ABSTRACT]) {
                return Some(e);
            }
            if is_member {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_CLASS_ELEMENTS_OF_THIS_KIND_1031);
            }
            if param {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_PARAMETER_1090);
            }
            if block {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
        }
        Modifiers::DECLARE => {
            if let Some(m) = first_seen(seen, &[Modifiers::ASYNC, Modifiers::OVERRIDE]) {
                return Some(err(
                    span,
                    dg::_0_MODIFIER_CANNOT_BE_USED_IN_AN_AMBIENT_CONTEXT_1040,
                    modifier_text(m),
                    "",
                ));
            }
            if is_member {
                if !member(Member::Property) {
                    return named(
                        dg::_0_MODIFIER_CANNOT_APPEAR_ON_CLASS_ELEMENTS_OF_THIS_KIND_1031,
                    );
                }
                if private_name {
                    return named(dg::_0_MODIFIER_CANNOT_BE_USED_WITH_A_PRIVATE_IDENTIFIER_18019);
                }
                if seen.contains(Modifiers::ACCESSOR) {
                    return with(span, "declare", "accessor");
                }
            }
            if param {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_PARAMETER_1090);
            }
            if block {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
            if let Site::Stmt {
                place: Place::Namespace,
                ambient: true,
                ..
            } = site
            {
                return here(
                    dg::A_DECLARE_MODIFIER_CANNOT_BE_USED_IN_AN_ALREADY_AMBIENT_CONTEXT_1038,
                );
            }
        }
        Modifiers::ASYNC => {
            let ambient = seen.contains(Modifiers::DECLARE)
                || matches!(
                    site,
                    Site::Member { ambient: true, .. } | Site::Stmt { ambient: true, .. }
                );
            if ambient {
                return named(dg::_0_MODIFIER_CANNOT_BE_USED_IN_AN_AMBIENT_CONTEXT_1040);
            }
            if param {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_PARAMETER_1090);
            }
            if let Site::Stmt {
                kind,
                place: Place::Block,
                ..
            } = site
                && kind != NodeKind::FunctionDecl
            {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
            if is_member && seen.contains(Modifiers::ABSTRACT) {
                return with(span, "async", "abstract");
            }
        }
        Modifiers::READONLY => {
            if seen.contains(Modifiers::ACCESSOR) {
                return with(span, "readonly", "accessor");
            }
            if block {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
            let ok = member(Member::Property)
                || param
                || matches!(
                    site,
                    Site::ClassIndex | Site::TypeIndex | Site::TypeMember { method: false }
                );
            if !ok {
                return here(
                    dg::READONLY_MODIFIER_CAN_ONLY_APPEAR_ON_A_PROPERTY_DECLARATION_OR_INDEX_SIGNATURE_1024,
                );
            }
        }
        Modifiers::ABSTRACT => {
            if let Site::Stmt { kind, place, .. } = site {
                if kind == NodeKind::ClassDecl {
                    return None;
                }
                if place == Place::Block {
                    return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
                }
                return here(
                    dg::ABSTRACT_MODIFIER_CAN_ONLY_APPEAR_ON_A_CLASS_METHOD_OR_PROPERTY_DECLARATION_1242,
                );
            }
            if param || member(Member::Constructor) {
                return here(
                    dg::ABSTRACT_MODIFIER_CAN_ONLY_APPEAR_ON_A_CLASS_METHOD_OR_PROPERTY_DECLARATION_1242,
                );
            }
            if let Site::Member {
                kind,
                abstract_class: false,
                ..
            } = site
            {
                return here(if kind == Member::Property {
                    dg::ABSTRACT_PROPERTIES_CAN_ONLY_APPEAR_WITHIN_AN_ABSTRACT_CLASS_1253
                } else {
                    dg::ABSTRACT_METHODS_CAN_ONLY_APPEAR_WITHIN_AN_ABSTRACT_CLASS_1244
                });
            }
            if private_name {
                return named(dg::_0_MODIFIER_CANNOT_BE_USED_WITH_A_PRIVATE_IDENTIFIER_18019);
            }
            if let Some(e) = order(&[Modifiers::OVERRIDE, Modifiers::ACCESSOR]) {
                return Some(e);
            }
            if is_member {
                if seen.contains(Modifiers::STATIC) {
                    return with(span, "static", "abstract");
                }
                if seen.contains(Modifiers::PRIVATE) {
                    return with(span, "private", "abstract");
                }
                if seen.contains(Modifiers::ASYNC) {
                    return with(async_span.unwrap_or(span), "async", "abstract");
                }
            }
        }
        Modifiers::ACCESSOR => {
            if let Some(m) = first_seen(seen, &[Modifiers::READONLY, Modifiers::DECLARE]) {
                return with(span, "accessor", modifier_text(m));
            }
            if block {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
            if !member(Member::Property) {
                return here(dg::ACCESSOR_MODIFIER_CAN_ONLY_APPEAR_ON_A_PROPERTY_DECLARATION_1275);
            }
        }
        Modifiers::OVERRIDE => {
            if let Some(e) = order(&[Modifiers::READONLY, Modifiers::ACCESSOR, Modifiers::ASYNC]) {
                return Some(e);
            }
            if seen.contains(Modifiers::DECLARE) {
                return with(span, "override", "declare");
            }
            if block {
                return here(dg::MODIFIERS_CANNOT_APPEAR_HERE_1184);
            }
            if member(Member::Constructor) {
                return named(dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_CONSTRUCTOR_DECLARATION_1089);
            }
        }
        _ => {}
    }
    None
}

/// `in` and `out` on a type parameter: a repeat is TS1030, `in` after `out` TS1029. Other
/// modifiers there are TS1273/TS1274/TS1277 (reported elsewhere), so those are left alone.
fn variance_error(toks: &[ts_r_ast::ModifierToken]) -> Option<ModifierError> {
    let variance = Modifiers::IN.union(Modifiers::OUT);
    if toks.iter().any(|t| !variance.contains(t.flag)) {
        return None;
    }
    let mut seen = Modifiers::NONE;
    for t in toks {
        let w = modifier_text(t.flag);
        if seen.contains(t.flag) {
            return Some(err(t.span, dg::_0_MODIFIER_ALREADY_SEEN_1030, w, ""));
        }
        if t.flag == Modifiers::IN && seen.contains(Modifiers::OUT) {
            return Some(err(
                t.span,
                dg::_0_MODIFIER_MUST_PRECEDE_1_MODIFIER_1029,
                "in",
                "out",
            ));
        }
        seen.insert(t.flag);
    }
    None
}

/// The span of the `?` after an optional property's name.
fn question_after(ast: &Ast, text: &str, key: NodeId) -> Option<Span> {
    let from = ast.span(key).end as usize;
    let rest = text.get(from..)?;
    let off = rest.len() - rest.trim_start().len();
    rest[off..]
        .starts_with('?')
        .then(|| Span::new((from + off) as u32, (from + off + 1) as u32))
}

impl<'c, 'a> Ck<'c, 'a> {
    fn modifier_site(&self, file: FileId, n: NodeId) -> Option<Site> {
        let ast = self.ast(file);
        let parent = self.c.file(file).parents().parent(n);
        let pnode = parent.map(|p| *ast.node(p));
        let class_abstract = match pnode {
            Some(Node::ClassDecl { modifiers, .. }) => {
                Some(modifiers.contains(Modifiers::ABSTRACT))
            }
            Some(Node::ClassExpr { .. }) => Some(false),
            _ => None,
        };
        let member = |kind: Member, key: NodeId| {
            let class = Loc::new(file, parent?);
            Some(Site::Member {
                kind,
                private: matches!(ast.node(key), Node::PrivateName { .. }),
                ambient: self.is_ambient_node(class),
                abstract_class: class_abstract?,
            })
        };
        match *ast.node(n) {
            Node::PropertyDef { key, .. } if class_abstract.is_some() => {
                member(Member::Property, key)
            }
            Node::MethodDef { kind, key, .. } if class_abstract.is_some() => {
                let kind = match kind {
                    MethodKind::Constructor => Member::Constructor,
                    MethodKind::Get | MethodKind::Set => Member::Accessor,
                    _ => Member::Method,
                };
                member(kind, key)
            }
            Node::IndexSignature { .. } if class_abstract.is_some() => Some(Site::ClassIndex),
            Node::IndexSignature { .. } => Some(Site::TypeIndex),
            Node::PropertySignature { .. } => Some(Site::TypeMember { method: false }),
            Node::MethodSignature { .. } => Some(Site::TypeMember { method: true }),
            // Modifiers on `this` parameters are TS1433 instead.
            Node::Param { .. } if crate::nodes::is_this_param(ast, n) => None,
            Node::Param { .. } => Some(Site::Param),
            Node::StaticBlock { .. } => Some(Site::StaticBlock),
            Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Enum { .. }
            | Node::Module { .. }
            | Node::ImportEquals { .. }
            | Node::ImportDecl { .. } => {
                let place = match pnode {
                    Some(Node::Program { .. }) => Place::Top,
                    Some(Node::ModuleBlock { .. }) => Place::Namespace,
                    _ => Place::Block,
                };
                // A namespace or import in a block is TS1235/TS1234/TS1232 instead.
                if place == Place::Block
                    && matches!(
                        ast.node(n),
                        Node::Module { .. } | Node::ImportEquals { .. } | Node::ImportDecl { .. }
                    )
                {
                    return None;
                }
                let ambient = parent.is_some_and(|p| self.is_ambient_node(Loc::new(file, p)));
                Some(Site::Stmt {
                    kind: ast.kind(n),
                    place,
                    ambient,
                })
            }
            _ => None,
        }
    }

    /// The first modifier error of declaration `n` (see the module docs), if any.
    pub(crate) fn modifier_error(&self, file: FileId, n: NodeId) -> Option<ModifierError> {
        let ast = self.ast(file);
        let toks = ast.modifier_tokens_of(n);
        if toks.is_empty() {
            return None;
        }
        if let Node::TypeParam { .. } = ast.node(n) {
            return variance_error(toks);
        }
        // A JSDoc `@typedef` alias has no modifiers; its span takes in the type's keywords
        // (`{{ readonly [k: string]: T }}`).
        if let Node::TypeAlias { modifiers, .. } = *ast.node(n)
            && modifiers.is_empty()
        {
            return None;
        }
        let site = self.modifier_site(file, n)?;
        let mut seen = Modifiers::NONE;
        let mut async_span = None;
        let skipped = Modifiers::CONST
            .union(Modifiers::DEFAULT)
            .union(Modifiers::IN)
            .union(Modifiers::OUT);
        for t in toks {
            if t.flag.intersects(skipped) {
                continue;
            }
            if let Some(e) = keyword_error(site, t.flag, t.span, seen, async_span) {
                return Some(e);
            }
            if t.flag == Modifiers::ASYNC {
                async_span = Some(t.span);
            }
            seen.insert(t.flag);
        }
        if let Some(at) = async_span {
            match site {
                Site::Member {
                    kind: Member::Constructor,
                    ..
                } => {
                    return Some(err(
                        at,
                        dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_CONSTRUCTOR_DECLARATION_1089,
                        "async",
                        "",
                    ));
                }
                Site::Member {
                    kind: Member::Property | Member::Accessor,
                    ..
                } => {
                    return Some(err(
                        at,
                        dg::_0_MODIFIER_CANNOT_BE_USED_HERE_1042,
                        "async",
                        "",
                    ));
                }
                Site::Stmt { kind, .. } if kind != NodeKind::FunctionDecl => {
                    return Some(err(
                        at,
                        dg::_0_MODIFIER_CANNOT_BE_USED_HERE_1042,
                        "async",
                        "",
                    ));
                }
                _ => {}
            }
        }
        if let Node::PropertyDef {
            key,
            optional: true,
            modifiers,
            ..
        } = *ast.node(n)
            && modifiers.contains(Modifiers::ACCESSOR)
            && let Some(q) = question_after(ast, &self.c.file(file).text, key)
        {
            return Some(err(
                q,
                dg::AN_ACCESSOR_PROPERTY_CANNOT_BE_DECLARED_OPTIONAL_1276,
                "",
                "",
            ));
        }
        // `declare import ..`: TS1079 at `declare`; `export import { a } from "m"`: TS1191
        // at `export`, when nothing else was wrong.
        if matches!(
            ast.node(n),
            Node::ImportDecl { .. } | Node::ImportEquals { .. }
        ) && let Some(t) = toks.iter().find(|t| t.flag == Modifiers::DECLARE)
        {
            return Some(err(
                t.span,
                dg::A_0_MODIFIER_CANNOT_BE_USED_WITH_AN_IMPORT_DECLARATION_1079,
                "declare",
                "",
            ));
        }
        if let Node::ImportDecl { .. } = ast.node(n)
            && let Some(t) = toks.iter().find(|t| t.flag == Modifiers::EXPORT)
        {
            return Some(err(
                t.span,
                dg::AN_IMPORT_DECLARATION_CANNOT_HAVE_MODIFIERS_1191,
                "",
                "",
            ));
        }
        None
    }

    /// Reports the modifier error of declaration `n`, if any.
    pub(crate) fn check_declaration_modifiers(&mut self, file: FileId, n: NodeId) {
        if let Some((span, info, args)) = self.modifier_error(file, n) {
            let k = args.iter().filter(|a| !a.is_empty()).count();
            self.error_span(file, span, info, &args[..k]);
        }
    }

    /// TS1046: the first top-level declaration of a `.d.ts` file without `declare` or
    /// `export`, at its start.
    pub(crate) fn check_dts_top_level_modifiers(&mut self, file: FileId) {
        if !self.c.file(file).is_declaration() {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        for &s in ast.list(stmts) {
            let modifiers = match *ast.node(s) {
                Node::VarDecl { modifiers, .. }
                | Node::FunctionDecl { modifiers, .. }
                | Node::ClassDecl { modifiers, .. }
                | Node::Enum { modifiers, .. }
                | Node::Module { modifiers, .. } => modifiers,
                _ => continue,
            };
            if !modifiers.intersects(Modifiers::DECLARE.union(Modifiers::EXPORT)) {
                self.error_span(
                    file,
                    ast.span(s),
                    dg::TOP_LEVEL_DECLARATIONS_IN_D_TS_FILES_MUST_START_WITH_EITHER_A_DECLARE_OR_EXPORT_M_1046,
                    &[],
                );
                return;
            }
        }
    }

    /// Modifiers on a type parameter, at the first one that breaks a rule (tsc 7): `const`
    /// on an interface's or type alias's is TS1277; `in`/`out` other than on a class,
    /// interface or type alias TS1274; any other modifier keyword TS1273. The keywords are
    /// read from the text before the name (JSDoc `@template` parameters included).
    pub(crate) fn check_type_param_modifiers(&mut self, file: FileId, tp: NodeId) {
        let ast = self.ast(file);
        let Node::TypeParam { name, .. } = *ast.node(tp) else {
            return;
        };
        let from = ast.span(tp).start as usize;
        let to = ast.span(name).start as usize;
        if from >= to {
            return;
        }
        let parents = self.c.file(file).parents();
        let owner = parents.parent(tp).and_then(|list| {
            if matches!(ast.node(list), Node::TypeParam { .. }) {
                None
            } else if ast.kind(list) == NodeKind::TypeParams {
                parents.parent(list)
            } else {
                Some(list)
            }
        });
        let alias_like = owner.is_some_and(|o| {
            matches!(ast.node(o), Node::Interface { .. } | Node::TypeAlias { .. })
        });
        let variance_ok = owner.is_some_and(|o| {
            matches!(
                ast.node(o),
                Node::Interface { .. }
                    | Node::TypeAlias { .. }
                    | Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
            )
        });
        let text = &self.c.file(file).text;
        let Some(src) = text.get(from..to) else {
            return;
        };
        let mut pos = 0;
        for w in src.split(|c: char| c.is_whitespace()) {
            let start = (from + pos) as u32;
            pos += w.len() + 1;
            if w.is_empty() {
                continue;
            }
            let span = Span::new(start, start + w.len() as u32);
            let msg = match w {
                "const" if alias_like => {
                    dg::_0_MODIFIER_CAN_ONLY_APPEAR_ON_A_TYPE_PARAMETER_OF_A_FUNCTION_METHOD_OR_CLASS_1277
                }
                "const" => continue,
                "in" | "out" if !variance_ok => {
                    dg::_0_MODIFIER_CAN_ONLY_APPEAR_ON_A_TYPE_PARAMETER_OF_A_CLASS_INTERFACE_OR_TYPE_ALIA_1274
                }
                "in" | "out" => continue,
                "public" | "private" | "protected" | "readonly" | "static" | "abstract"
                | "async" | "declare" | "export" | "default" | "override" | "accessor" => {
                    dg::_0_MODIFIER_CANNOT_APPEAR_ON_A_TYPE_PARAMETER_1273
                }
                _ => return,
            };
            self.error_span(file, span, msg, &[w]);
            return;
        }
    }
}
