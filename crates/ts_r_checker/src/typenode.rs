//! Types as declaration-file type nodes (design record 0012, E2): [`Ck::type_to_type_node`].
//!
//! The declaration emitter asks for the type of every declaration whose annotation is
//! missing and gets back nodes appended to its own copy of the file's AST (synthesized nodes
//! with empty spans). The printed forms follow tsc 7.0.2's declaration output (observed):
//! the same forms as the type printer (`print.rs`: unions with `null`/`undefined` last,
//! `boolean` and whole enums collapsed, arrays as `T[]`), plus what declarations need:
//!
//! - names are written so that they resolve from the declaration (`m.c`, an import's
//!   local name, `import("./m").T` when nothing in scope names the symbol);
//! - functions and classes that can be named print as `typeof f` / `typeof C`; others print
//!   structurally (a class instance type that cannot be named as its members);
//! - nothing is truncated; a type that is still unresolved is an error
//!   ([`NotNameable::Unresolved`]): the emitter refuses the file rather than print `any`.
//!
//! The symbols each built name refers to are collected in [`TypeNodeCx::refs`], so the
//! emitter can keep the local declarations and imports those names need.

use ts_r_ast::{Ast, KeywordTypeKind as K, MethodKind, Modifiers, Node, NodeId, TypeOperatorKind};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_diagnostics::FileId;
use ts_r_span::Span;
use ts_r_types::{
    MapperId, ObjectFlags, ObjectKind, SigFlags, SigId, SymId, TupleElement, TypeFlags, TypeId,
    TypeKind,
};

use crate::checker::{Ck, Loc};
use crate::declared::anon;
use crate::members::SigKind;

mod reuse;
mod specifier;

use specifier::Ending;
pub use specifier::{ModuleSpecifierHost, specifier_host, specifier_host_with_references};

/// Nesting bound for built type nodes (a deeper type is [`NotNameable::TooDeep`]).
const MAX_NODE_DEPTH: u32 = 64;

/// Instantiations of one anonymous type's declaration that may be written inside each
/// other; the next one is `/*elided*/ any` (observed in tsc 7.0.2: a recursive generic
/// arrow or object literal writes its declared type and 11 nested instantiations, as in
/// `declarationsWithRecursiveInternalTypesProduceUniqueTypeParams`).
const MAX_INSTANTIATION_NESTING: usize = 11;

/// The approximate length past which a type written from its structure is TS7056 ("exceeds
/// the maximum length the compiler will serialize"). Counted as tsc appears to count
/// (observed): each written member or parameter is its name plus 2, each leaf type its
/// text, each other built type node 2; nodes written from source syntax (an object
/// literal's keys) are not counted. A spread of an interface with 66,500 `p000000: number`
/// members is written and one with 66,750 is TS7056: 15 per member.
pub const MAX_SERIALIZED_LENGTH: usize = 1_000_000;

/// Why a type could not be written as a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotNameable {
    /// Some part of the type is still unresolved (`TypeId::UNRESOLVED`, TSR2001).
    Unresolved,
    /// The type nests deeper than the emitter writes.
    TooDeep,
    /// The type written from its structure is longer than tsc serializes (TS7056; see
    /// [`MAX_SERIALIZED_LENGTH`]).
    TooLong,
    /// A symbol the type needs cannot be named from the declaration.
    Inaccessible(String),
    /// A symbol the type needs lives in a module the file can only reach through a
    /// `node_modules` path no import resolves (TS2883): `name` from `module`.
    NotPortable { name: String, module: String },
    /// An anonymous class type has private or protected members (TS4094, one per member,
    /// in member order).
    PrivateMember(Vec<String>),
    /// A recursive type whose only name is a type alias that another module does not
    /// export (TS4023 on a variable): `name` from `module` (`"/path/to/file"`).
    CannotBeNamed { name: String, module: String },
    /// A property keyed by a unique symbol that no declaration names (a mapped type's
    /// `[K in typeof sym]`): TS4118, `[sym]`.
    NonSerializableProperty(String),
}

/// Where built nodes go and what they referenced.
pub struct TypeNodeCx<'o> {
    /// The emitter's copy of the file's AST; nodes are appended.
    pub out: &'o mut Ast,
    /// The declaration the type is written at (names must resolve from here).
    pub enclosing: Loc,
    /// Symbols that built names refer to (aliases as the alias symbol), in order.
    pub refs: Vec<SymId>,
    /// Package lookups for `import("pkg")` specifiers (relative paths only without).
    pub host: Option<&'o dyn ModuleSpecifierHost>,
    /// Built members of object literal types and the object literal members that declare
    /// them (whose comments tsc copies onto the written member).
    pub member_decls: Vec<(NodeId, Loc)>,
    /// The last export that could not be named portably (`name`, module path).
    not_portable: Option<(String, String)>,
    stack: Vec<TypeId>,
    depth: u32,
    /// Approximate length written from types so far ([`MAX_SERIALIZED_LENGTH`]).
    approx_len: usize,
    /// Type parameters of the signatures (and `infer` types) being printed and their
    /// written names (`T_1` for a `T` that shadows another).
    tp_names: Vec<(TypeId, String)>,
    /// Built nodes that stand for source text of the enclosing file (object literal keys
    /// written from the literal's syntax), with that text's span: the declaration map maps
    /// them there.
    pub origins: Vec<(NodeId, Span)>,
    /// The object literal of the enclosing file whose keys the next written structure keeps
    /// (set by the expression path, taken by the structure).
    literal_keys: Option<NodeId>,
    /// A recursive type named only by another module's unexported alias is an error
    /// ([`NotNameable::CannotBeNamed`]) rather than `/*elided*/ any` (set for variables).
    pub report_unnameable: bool,
    /// Names given to `infer` type parameters so far: one name per type parameter for the
    /// whole written type, and a name once given is not reused by another (observed:
    /// sibling conditionals write `infer T`, `infer T_1`, `infer T_2`).
    infer_names: Vec<(TypeId, String)>,
}

impl std::fmt::Debug for TypeNodeCx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeNodeCx")
            .field("enclosing", &self.enclosing)
            .field("refs", &self.refs)
            .field("host", &self.host.is_some())
            .finish_non_exhaustive()
    }
}

impl<'o> TypeNodeCx<'o> {
    pub fn new(out: &'o mut Ast, enclosing: Loc) -> TypeNodeCx<'o> {
        TypeNodeCx {
            out,
            enclosing,
            refs: Vec::new(),
            host: None,
            member_decls: Vec::new(),
            not_portable: None,
            stack: Vec::new(),
            depth: 0,
            approx_len: 0,
            tp_names: Vec::new(),
            origins: Vec::new(),
            literal_keys: None,
            report_unnameable: false,
            infer_names: Vec::new(),
        }
    }

    /// Names packages through `host` ([`ModuleSpecifierHost`]).
    pub fn with_host(mut self, host: Option<&'o dyn ModuleSpecifierHost>) -> TypeNodeCx<'o> {
        self.host = host;
        self
    }

    fn add(&mut self, n: Node) -> NodeId {
        self.out.add(n, Span::EMPTY)
    }

    fn ident(&mut self, name: &str) -> NodeId {
        let a = self.out.intern(name);
        self.add(Node::Ident { name: a })
    }

    fn kw(&mut self, kind: K) -> NodeId {
        self.add(Node::KeywordType { kind })
    }

    /// `/*elided*/ any`: an anonymous type met again inside itself (observed). A type
    /// reference whose name carries the comment, so printers write it as it is.
    fn elided(&mut self) -> NodeId {
        let name = self.ident("/*elided*/ any");
        self.add(Node::TypeRef {
            name,
            type_args: None,
        })
    }

    fn type_ref(&mut self, name: NodeId, args: &[NodeId]) -> NodeId {
        let type_args = if args.is_empty() {
            None
        } else {
            let l = self.out.add_list(args);
            Some(self.add(Node::TypeArgs { args: l }))
        };
        self.add(Node::TypeRef { name, type_args })
    }

    /// A property name, mapped to `origin` (the source key) when it has one.
    fn key(&mut self, name: &str, origin: Option<Span>) -> NodeId {
        let k = self.ident(name);
        if let Some(sp) = origin {
            self.origins.push((k, sp));
        }
        k
    }

    fn paren(&mut self, ty: NodeId) -> NodeId {
        self.add(Node::ParenType { ty })
    }

    fn string_lit(&mut self, s: &str) -> NodeId {
        let a = self.out.intern(s);
        self.add(Node::StringLit { value: a })
    }
}

type Built = Result<NodeId, NotNameable>;

/// [`NotNameable::NotPortable`] when naming failed for want of a portable specifier since
/// `before` (whether one had failed already) was taken.
fn unportable_since(before: bool, cx: &TypeNodeCx) -> Option<NotNameable> {
    if before {
        return None;
    }
    let (name, module) = cx.not_portable.clone()?;
    Some(NotNameable::NotPortable { name, module })
}

/// Whether type node `n` holds an `import()` type with `assert` attributes. tsc 7 does not
/// reuse such a node for an inferred type; it writes the type (observed:
/// `null as any as import("pkg", { assert: {} }).I` is `import("pkg").I`).
pub fn has_assert_import_type(ast: &Ast, n: NodeId) -> bool {
    use ts_r_ast::visit::{Visitor, Walk, walk};
    struct Finder(bool);
    impl Visitor for Finder {
        fn enter(&mut self, ast: &Ast, n: NodeId) -> Walk {
            if let Node::ImportType {
                attributes: Some(a),
                ..
            } = *ast.node(n)
                && matches!(ast.node(a), Node::ImportAttributes { assert: true, .. })
            {
                self.0 = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Finder(false);
    walk(ast, n, &mut f);
    f.0
}

pub fn is_identifier_text(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

impl<'c, 'a> Ck<'c, 'a> {
    /// `t` as a type node in `cx.out`, written to resolve from `cx.enclosing`.
    pub fn type_to_type_node(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        cx.not_portable = None;
        cx.approx_len = 0;
        match self.tn(t, cx) {
            Err(NotNameable::Inaccessible(n)) => match cx.not_portable.take() {
                Some((name, module)) => Err(NotNameable::NotPortable { name, module }),
                None => Err(NotNameable::Inaccessible(n)),
            },
            r => r,
        }
    }

    /// `t`, the type of expression `e` of the enclosing file, as a type node: parts tsc
    /// writes from the expression's syntax (an object literal's keys, `as T`, literals of a
    /// `const` context) are written from it (see `typenode/reuse.rs`).
    pub fn expr_type_to_type_node(&mut self, e: Loc, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        cx.not_portable = None;
        cx.approx_len = 0;
        match self.expr_tn(e, t, cx) {
            Err(NotNameable::Inaccessible(n)) => match cx.not_portable.take() {
                Some((name, module)) => Err(NotNameable::NotPortable { name, module }),
                None => Err(NotNameable::Inaccessible(n)),
            },
            r => r,
        }
    }

    /// Whether `s` is declared in the body of function `at`: a type written in the
    /// function's signature cannot name it (`typeof inner` of a local is its structure).
    fn in_body_of(&self, at: Loc, s: SymId) -> bool {
        let ast = self.ast(at.file);
        let Some(body) = crate::nodes::fn_parts(ast, at.node).and_then(|p| p.body) else {
            return false;
        };
        let b = ast.span(body);
        self.c.first_decl(s).is_some_and(|d| {
            d.file == at.file && b.start <= ast.span(d.node).start && ast.span(d.node).start < b.end
        })
    }

    /// The symbol `name` resolves to at `at` with `meaning` (lexical scopes, merged
    /// namespaces, then globals).
    pub fn resolve_name_at(&self, at: Loc, name: &str, m: u8) -> Option<SymId> {
        self.c
            .lookup_in_scope_chain(at, name, m)
            .or_else(|| self.c.lookup_in_merged_namespaces(at, name, m))
            .or_else(|| self.c.global_symbol(name, m))
    }

    fn tn(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        if cx.depth > MAX_NODE_DEPTH {
            return Err(NotNameable::TooDeep);
        }
        if cx.approx_len > MAX_SERIALIZED_LENGTH {
            return Err(NotNameable::TooLong);
        }
        cx.depth += 1;
        let r = self.tn_inner(t, cx);
        cx.depth -= 1;
        if let Ok(n) = r {
            cx.approx_len += leaf_len(cx.out, n);
        }
        r
    }

    fn tn_inner(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        let s = self.store();
        match t {
            TypeId::UNRESOLVED => return Err(NotNameable::Unresolved),
            TypeId::ERROR | TypeId::IMPLICIT_ANY | TypeId::AUTO | TypeId::WILDCARD => {
                return Ok(cx.kw(K::Any));
            }
            TypeId::MISSING => return Ok(cx.kw(K::Undefined)),
            TypeId::EMPTY_OBJECT | TypeId::EMPTY_FRESH_OBJECT => {
                return Ok(cx.add(Node::TypeLiteral {
                    members: ts_r_ast::NodeList::default(),
                }));
            }
            _ => {}
        }
        if Some(t) == self.global_this {
            let n = cx.ident("globalThis");
            return Ok(cx.add(Node::TypeQuery {
                expr_name: n,
                type_args: None,
            }));
        }
        if let Some(name) = s.error_name(t) {
            let name = name.to_owned();
            let n = self.entity_from_text(&name, cx);
            return Ok(cx.type_ref(n, &[]));
        }
        // An exported alias that has no portable name is an error, not its structure.
        let portable_before = cx.not_portable.is_some();
        if let Some(sym) = s.forward_alias(t)
            && let Some(n) = self.entity_of(sym, meaning::TYPE, cx)
        {
            return Ok(cx.type_ref(n, &[]));
        }
        if let Some(info) = s.alias_of(t) {
            let args = s.list_items(info.args).to_vec();
            if let Some(n) = self.entity_of(info.sym, meaning::TYPE, cx) {
                let mut built = Vec::with_capacity(args.len());
                for a in args {
                    built.push(self.tn(a, cx)?);
                }
                return Ok(cx.type_ref(n, &built));
            }
        }
        if let Some(e) = unportable_since(portable_before, cx) {
            return Err(e);
        }
        let f = s.flags(t);
        match s.kind(t) {
            TypeKind::Intrinsic => {
                let k = if f.contains(TypeFlags::ANY) {
                    K::Any
                } else if f.contains(TypeFlags::UNKNOWN) {
                    K::Unknown
                } else if f.contains(TypeFlags::NEVER) {
                    K::Never
                } else if f.contains(TypeFlags::VOID) {
                    K::Void
                } else if f.contains(TypeFlags::UNDEFINED) {
                    K::Undefined
                } else if f.contains(TypeFlags::NULL) {
                    K::Null
                } else if f.contains(TypeFlags::STRING) {
                    K::String
                } else if f.contains(TypeFlags::NUMBER) {
                    K::Number
                } else if f.contains(TypeFlags::BIGINT) {
                    K::BigInt
                } else if f.contains(TypeFlags::ES_SYMBOL) {
                    K::Symbol
                } else if f.contains(TypeFlags::NON_PRIMITIVE) {
                    K::Object
                } else if f.contains(TypeFlags::BOOLEAN_LITERAL) {
                    let value = s.regular(t) == TypeId::TRUE;
                    return Ok(cx.add(Node::BoolLit { value }));
                } else {
                    K::Any
                };
                Ok(cx.kw(k))
            }
            TypeKind::StringLiteral(x) => {
                let v = s.str(x).to_owned();
                Ok(cx.string_lit(&v))
            }
            TypeKind::NumberLiteral(..) => {
                let v = s.number_value(t).unwrap_or(0.0);
                Ok(number_node(cx, v))
            }
            TypeKind::BigIntLiteral(x) => {
                let v = s.str(x).to_owned();
                Ok(bigint_node(cx, &v))
            }
            TypeKind::EnumLiteral { member, .. } => self.enum_literal_node(member, cx),
            TypeKind::Enum(sym) => self.named(sym, meaning::TYPE, cx),
            TypeKind::UniqueSymbol(sym) => {
                let Some(n) = self.entity_of(sym, meaning::VALUE, cx) else {
                    // Not nameable here: tsc writes the widened `symbol`.
                    return Ok(cx.kw(K::Symbol));
                };
                Ok(cx.add(Node::TypeQuery {
                    expr_name: n,
                    type_args: None,
                }))
            }
            TypeKind::TypeParameter(_) => {
                let d = s.type_param_data(t);
                match d {
                    Some(d) if d.is_this => Ok(cx.add(Node::ThisType {})),
                    Some(d) => {
                        let name = match cx.tp_names.iter().rev().find(|(p, _)| *p == t) {
                            Some((_, n)) => n.clone(),
                            None => self.c.sym_name(d.sym).to_owned(),
                        };
                        if self.print_infer.contains(&t) {
                            let n = cx.ident(&name);
                            let constraint = match self.type_param_constraint(t) {
                                Some(c) => Some(self.tn(c, cx)?),
                                None => None,
                            };
                            let tp = cx.add(Node::TypeParam {
                                modifiers: Modifiers::NONE,
                                name: n,
                                constraint,
                                default: None,
                            });
                            return Ok(cx.add(Node::InferType { type_param: tp }));
                        }
                        let n = cx.ident(&name);
                        Ok(cx.type_ref(n, &[]))
                    }
                    None => Err(NotNameable::Unresolved),
                }
            }
            TypeKind::Union { list, .. } => {
                let members = match s.union_origin(t) {
                    Some(&[one]) => return self.tn(one, cx),
                    Some(o) => o.to_vec(),
                    None => s.list_items(list).to_vec(),
                };
                self.union_node(&members, cx)
            }
            TypeKind::Intersection { list, .. } => {
                let members = s.list_items(list).to_vec();
                let mut parts = Vec::with_capacity(members.len());
                for m in members {
                    parts.push(self.wrapped(m, cx, true)?);
                }
                let l = cx.out.add_list(&parts);
                Ok(cx.add(Node::IntersectionType { types: l }))
            }
            TypeKind::Index(x) => {
                let ty = self.wrapped(x, cx, true)?;
                Ok(cx.add(Node::TypeOperator {
                    op: TypeOperatorKind::KeyOf,
                    ty,
                }))
            }
            TypeKind::IndexedAccess { obj, index } => {
                let obj = self.wrapped(obj, cx, true)?;
                let index = self.tn(index, cx)?;
                Ok(cx.add(Node::IndexedAccessType { obj, index }))
            }
            TypeKind::Substitution { base, .. } => self.tn(base, cx),
            TypeKind::StringMapping { sym, ty } => {
                let name = self.c.sym_name(sym).to_owned();
                let n = cx.ident(&name);
                let arg = self.tn(ty, cx)?;
                Ok(cx.type_ref(n, &[arg]))
            }
            TypeKind::Conditional(_) => self.conditional_node(t, cx),
            TypeKind::Forward(_) => Err(NotNameable::Unresolved),
            TypeKind::TemplateLiteral(_) => {
                let Some((texts, types)) = s.template_parts(t) else {
                    return Ok(cx.kw(K::String));
                };
                let texts: Vec<String> = texts
                    .iter()
                    .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                    .collect();
                let types = types.to_vec();
                let mut parts = Vec::with_capacity(texts.len() + types.len());
                for (i, text) in texts.iter().enumerate() {
                    let a = cx.out.intern(text);
                    parts.push(cx.add(Node::TemplateElement {
                        cooked: a,
                        invalid_escape: false,
                    }));
                    if let Some(&ty) = types.get(i) {
                        parts.push(self.tn(ty, cx)?);
                    }
                }
                let l = cx.out.add_list(&parts);
                Ok(cx.add(Node::TemplateLitType { parts: l }))
            }
            TypeKind::Object(_) => self.object_node(t, cx),
        }
    }

    /// `t` where a function, constructor, union (`wrap_union`), `keyof` or conditional
    /// type needs parentheses (array elements, union and intersection members).
    fn wrapped(&mut self, t: TypeId, cx: &mut TypeNodeCx, wrap_union: bool) -> Built {
        let n = self.tn(t, cx)?;
        Ok(if needs_parens(cx.out, n, wrap_union) {
            cx.paren(n)
        } else {
            n
        })
    }

    fn union_node(&mut self, members: &[TypeId], cx: &mut TypeNodeCx) -> Built {
        let s = self.store();
        let has_false = members.iter().any(|&m| s.regular(m) == TypeId::FALSE);
        let has_true = members.iter().any(|&m| s.regular(m) == TypeId::TRUE);
        let collapse_bool = has_false && has_true;
        let mut enum_groups: Vec<(SymId, Vec<TypeId>)> = Vec::new();
        for &m in members {
            if let TypeKind::EnumLiteral { member, .. } = s.kind(m)
                && let Some(p) = self.c.sym_parent(member)
            {
                match enum_groups.iter_mut().find(|g| g.0 == p) {
                    Some(g) => g.1.push(s.regular(m)),
                    None => enum_groups.push((p, vec![s.regular(m)])),
                }
            }
        }
        let mut collapsed: Vec<SymId> = Vec::new();
        for (e, lits) in &enum_groups {
            let all = self.enum_literal_set_of(*e);
            if all.len() > 1 && all.iter().all(|x| lits.contains(x)) {
                collapsed.push(*e);
            }
        }
        let mut parts: Vec<NodeId> = Vec::new();
        let mut enums: Vec<NodeId> = Vec::new();
        let mut printed_enums: Vec<SymId> = Vec::new();
        let (mut has_null, mut has_undefined) = (false, false);
        for &m in members {
            let f = s.flags(m);
            if f.contains(TypeFlags::NULL) {
                has_null = true;
                continue;
            }
            if f.contains(TypeFlags::UNDEFINED) {
                has_undefined = true;
                continue;
            }
            if collapse_bool && f.contains(TypeFlags::BOOLEAN_LITERAL) {
                if s.regular(m) == TypeId::FALSE {
                    parts.push(cx.kw(K::Boolean));
                }
                continue;
            }
            if let TypeKind::EnumLiteral { member, .. } = s.kind(m)
                && let Some(p) = self.c.sym_parent(member)
                && collapsed.contains(&p)
            {
                if !printed_enums.contains(&p) {
                    printed_enums.push(p);
                    enums.push(self.named(p, meaning::TYPE, cx)?);
                }
                continue;
            }
            let n = self.tn(m, cx)?;
            let n = if needs_parens_in_union(cx.out, n) {
                cx.paren(n)
            } else {
                n
            };
            parts.push(n);
        }
        parts.extend(enums);
        if has_null {
            cx.approx_len += 4;
            parts.push(cx.kw(K::Null));
        }
        if has_undefined {
            cx.approx_len += 9;
            parts.push(cx.kw(K::Undefined));
        }
        if parts.len() == 1 {
            return Ok(parts[0]);
        }
        let l = cx.out.add_list(&parts);
        Ok(cx.add(Node::UnionType { types: l }))
    }

    fn enum_literal_set_of(&mut self, e: SymId) -> Vec<TypeId> {
        let d = self.declared_type_of_symbol(e);
        let s = self.store();
        if s.flags(d).contains(TypeFlags::UNION) {
            s.members_of_union(d)
                .iter()
                .map(|&m| s.regular(m))
                .collect()
        } else {
            vec![s.regular(d)]
        }
    }

    fn enum_literal_node(&mut self, member: SymId, cx: &mut TypeNodeCx) -> Built {
        let Some(parent) = self.c.sym_parent(member) else {
            let name = self.c.sym_name(member).to_owned();
            let n = cx.ident(&name);
            return Ok(cx.type_ref(n, &[]));
        };
        let members = self
            .c
            .exports_of(parent)
            .into_iter()
            .filter(|(_, m)| self.c.sym_flags(*m).contains(SymbolFlags::ENUM_MEMBER))
            .count();
        let Some(e) = self.entity_of(parent, meaning::TYPE | meaning::VALUE, cx) else {
            return Err(NotNameable::Inaccessible(
                self.c.sym_name(parent).to_owned(),
            ));
        };
        if members == 1 {
            return Ok(cx.type_ref(e, &[]));
        }
        let mname = self.c.sym_name(member).to_owned();
        if is_identifier_text(&mname) {
            let right = cx.ident(&mname);
            let q = cx.add(Node::QualifiedName { left: e, right });
            Ok(cx.type_ref(q, &[]))
        } else {
            let obj = cx.type_ref(e, &[]);
            let index = cx.string_lit(&mname);
            Ok(cx.add(Node::IndexedAccessType { obj, index }))
        }
    }

    /// A reference to `sym` by name (`TypeRef`), or `Inaccessible`.
    fn named(&mut self, sym: SymId, m: u8, cx: &mut TypeNodeCx) -> Built {
        match self.entity_of(sym, m, cx) {
            Some(n) => Ok(cx.type_ref(n, &[])),
            None => Err(NotNameable::Inaccessible(self.c.sym_name(sym).to_owned())),
        }
    }

    /// An entity name for `sym` with `meaning` that resolves from `cx.enclosing`: its own
    /// name when in scope, else qualified through its parent namespace or enum, else an
    /// import type. Records the referenced symbols. `None` when it cannot be named.
    pub fn entity_of(&mut self, sym: SymId, m: u8, cx: &mut TypeNodeCx) -> Option<NodeId> {
        self.entity_of_depth(sym, m, cx, 0)
    }

    fn entity_of_depth(
        &mut self,
        sym: SymId,
        m: u8,
        cx: &mut TypeNodeCx,
        depth: u32,
    ) -> Option<NodeId> {
        if depth > 16 {
            return None;
        }
        let portable_before = cx.not_portable.clone();
        if let Some(n) = self.entity_of_inner(sym, m, cx, depth) {
            return Some(n);
        }
        // `ns.Name` through a namespace import of a module that re-exports `sym`
        // (`import type * as y from "yargs"` whose `.d.mts` re-exports the `export =`
        // namespace's `Argv`).
        let n = self.namespace_import_entity(sym, cx)?;
        cx.not_portable = portable_before;
        Some(n)
    }

    /// A top-level namespace import (`import * as ns`, `import ns = require()`) of the
    /// enclosing file whose module (or its `export =` namespace) exports `sym` (directly or
    /// through an alias): `ns.Name`.
    fn namespace_import_entity(&mut self, sym: SymId, cx: &mut TypeNodeCx) -> Option<NodeId> {
        let file = cx.enclosing.file;
        let b = self.c.bound(file);
        let locals = b.scopes.first()?.locals;
        let found = b.table(locals).iter().find_map(|&(_, local)| {
            let id = self.c.sym(file, local);
            if !self.c.sym_flags(id).intersects(SymbolFlags::ALIAS) {
                return None;
            }
            // A default import of a module without a default export names the module
            // under `allowSyntheticDefaultImports` (observed: `http.Server`).
            let target = self
                .c
                .resolve_alias(id)
                .or_else(|| self.c.synthetic_default_module(id))?;
            if !self.c.sym_flags(target).intersects(
                SymbolFlags::MODULE_FILE
                    | SymbolFlags::NAMESPACE_MODULE
                    | SymbolFlags::VALUE_MODULE,
            ) {
                return None;
            }
            let mut exports = self.c.exports_of(target);
            // `export =` of a namespace: the import names the namespace's members.
            if let Some(ns) = exports
                .iter()
                .find(|(name, _)| name == "export=")
                .and_then(|&(_, e)| self.c.resolve_alias(e))
                && ns != sym
                && self
                    .c
                    .sym_flags(ns)
                    .intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
            {
                exports = self.c.exports_of(ns);
            }
            exports
                .into_iter()
                .find(|(name, e)| {
                    is_identifier_text(name)
                        && (*e == sym
                            || (self.c.sym_flags(*e).intersects(SymbolFlags::ALIAS)
                                && self.c.resolve_alias(*e) == Some(sym)))
                })
                .map(|(name, _)| (id, name))
        });
        let (alias, name) = found?;
        cx.refs.push(alias);
        let an = self.c.sym_name(alias).to_owned();
        let left = cx.ident(&an);
        let right = cx.ident(&name);
        Some(cx.add(Node::QualifiedName { left, right }))
    }

    fn entity_of_inner(
        &mut self,
        sym: SymId,
        m: u8,
        cx: &mut TypeNodeCx,
        depth: u32,
    ) -> Option<NodeId> {
        // A local alias (`import x = N`) in a scope searched before the one that declares
        // `sym` by name: tsc names it through the alias.
        if let Some((alias, qualified)) = self.local_alias_first(cx.enclosing, sym, m) {
            cx.refs.push(alias);
            let n = self.c.sym_name(alias).to_owned();
            let left = cx.ident(&n);
            if !qualified {
                return Some(left);
            }
            let rn = self.c.sym_name(sym).to_owned();
            let right = cx.ident(&rn);
            return Some(cx.add(Node::QualifiedName { left, right }));
        }
        let local = self.local_name_of(sym);
        if let Some(name) = &local {
            match self.resolve_name_at(cx.enclosing, name, m) {
                Some(found)
                    if self.same_symbol(found, sym) && !self.in_body_of(cx.enclosing, found) =>
                {
                    cx.refs.push(found);
                    return Some(cx.ident(name));
                }
                // Exported declarations of the enclosing module are in scope by their
                // names (they live in the module's export table, not its locals).
                None if self.is_own_export_of_file(cx.enclosing.file, sym, m) => {
                    cx.refs.push(sym);
                    return Some(cx.ident(name));
                }
                _ => {}
            }
        }
        // An import (or other alias) in the enclosing file that names it.
        if let Some(alias) = self.alias_in_file_for(cx.enclosing, sym, m) {
            cx.refs.push(alias);
            let n = self.c.sym_name(alias).to_owned();
            return Some(cx.ident(&n));
        }
        let Some(parent) = self.c.sym_parent(sym) else {
            return self.local_export_entity(sym, cx);
        };
        let pf = self.c.sym_flags(parent);
        if pf.intersects(SymbolFlags::MODULE_FILE) || self.is_ambient_module_symbol(parent) {
            // Members of a module are qualified by their export names (`default`).
            let export_name = self.c.sym_name(sym).to_owned();
            if export_name.starts_with("__") || export_name.starts_with('"') {
                return None;
            }
            return self.module_member_entity(sym, parent, &export_name, cx);
        }
        let name = local?;
        if pf.intersects(
            SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE | SymbolFlags::ENUM,
        ) {
            if let Some(n) = self.export_equals_member_entity(parent, &name, cx) {
                return Some(n);
            }
            let left =
                self.entity_of_depth(parent, meaning::NAMESPACE | meaning::VALUE, cx, depth + 1)?;
            let right = cx.ident(&name);
            return Some(cx.add(Node::QualifiedName { left, right }));
        }
        // A property of the type literal annotating a variable is named through the
        // variable (`Op.or` for `declare const Op: { readonly or: unique symbol }`).
        if pf.intersects(SymbolFlags::TYPE_LITERAL)
            && m & meaning::VALUE != 0
            && let Some(var) = self.annotated_variable(parent)
        {
            let left = self.entity_of_depth(var, meaning::VALUE, cx, depth + 1)?;
            let right = cx.ident(&name);
            return Some(cx.add(Node::QualifiedName { left, right }));
        }
        None
    }

    /// Member `name` of namespace `ns` when `ns` is the `export =` of a module and no alias
    /// of the enclosing file names it: `import("m").name`, even from the module itself
    /// (observed: `import("./es5ExportEqualsDts").B` for `A.B` beside `export = A`, and
    /// `import("ws").Server` for `ws`'s `export = WebSocket` namespace).
    fn export_equals_member_entity(
        &mut self,
        ns: SymId,
        name: &str,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        if !is_identifier_text(name) {
            return None;
        }
        let module = self.export_equals_module(ns)?;
        if self
            .alias_in_file_for(cx.enclosing, ns, meaning::NAMESPACE)
            .is_some()
            || self
                .alias_in_file_for(cx.enclosing, module, meaning::NAMESPACE)
                .is_some()
        {
            return None;
        }
        let spec = self.module_specifier_for(cx.enclosing.file, module, cx.host)?;
        let arg = cx.string_lit(&spec);
        let qualifier = cx.ident(name);
        Some(cx.add(Node::ImportType {
            is_typeof: false,
            arg,
            attributes: None,
            qualifier: Some(qualifier),
            type_args: None,
        }))
    }

    /// The module (a file or `declare module "m"`) whose `export =` is `ns`.
    fn export_equals_module(&self, ns: SymId) -> Option<SymId> {
        let module = match self.c.sym_parent(ns) {
            Some(p) if self.is_ambient_module_symbol(p) => p,
            Some(_) => return None,
            None => {
                let file = self.c.first_decl(ns)?.file;
                let m = self
                    .c
                    .module_target_symbol(ts_r_binder::program::ModuleTarget::File(file))?;
                if !self.c.sym_flags(m).intersects(SymbolFlags::MODULE_FILE) {
                    return None;
                }
                m
            }
        };
        self.c
            .exports_of(module)
            .iter()
            .any(|(n, e)| {
                n == "export="
                    && (*e == ns
                        || (self.c.sym_flags(*e).intersects(SymbolFlags::ALIAS)
                            && self.c.resolve_alias(*e) == Some(ns)))
            })
            .then_some(module)
    }

    /// The variable whose annotation is type literal `lit` (its symbol).
    fn annotated_variable(&self, lit: SymId) -> Option<SymId> {
        let d = self.c.first_decl(lit)?;
        let ast = self.ast(d.file);
        let p = self.c.file(d.file).parents().parent(d.node)?;
        match *ast.node(p) {
            Node::VarDeclarator {
                name,
                type_ann: Some(t),
                ..
            } if t == d.node && matches!(ast.node(name), Node::Ident { .. }) => self
                .c
                .sym_of_decl(Loc::new(d.file, p))
                .or_else(|| self.c.sym_of_decl(Loc::new(d.file, name))),
            _ => None,
        }
    }

    /// Member `sym` of module `parent`, exported as `export_name`, named from another file:
    /// `ns.Name` through a namespace import, else a re-exporting import, else an import
    /// type of the module.
    fn module_member_entity(
        &mut self,
        sym: SymId,
        parent: SymId,
        export_name: &str,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        // `ns.Name` through a namespace import or `import x = require()` of the module.
        if let Some(alias) = self.alias_in_file_for(cx.enclosing, parent, meaning::NAMESPACE) {
            cx.refs.push(alias);
            let an = self.c.sym_name(alias).to_owned();
            let left = cx.ident(&an);
            let right = cx.ident(export_name);
            return Some(cx.add(Node::QualifiedName { left, right }));
        }
        if let Some(n) = self.reexporting_import(sym, parent, cx) {
            return Some(n);
        }
        if let Some(n) = self.shorter_reexport(sym, parent, cx) {
            return Some(n);
        }
        self.import_type_entity(parent, export_name, cx)
    }

    /// A local declaration of another module that the module exports through an export
    /// declaration (`export { Suit }`, `export default Color`, `export = Conn`): named by
    /// its export name (`import("./color").default`), or the module itself for
    /// `export =` (`import("./foo")`), as observed.
    fn local_export_entity(&mut self, sym: SymId, cx: &mut TypeNodeCx) -> Option<NodeId> {
        let file = self.c.first_decl(sym)?.file;
        if file == cx.enclosing.file {
            return None;
        }
        let module = self
            .c
            .module_target_symbol(ts_r_binder::program::ModuleTarget::File(file))?;
        if !self
            .c
            .sym_flags(module)
            .intersects(SymbolFlags::MODULE_FILE)
        {
            return None;
        }
        let exports = self.c.exports_of(module);
        let exports_symbol = |ck: &Self, export: SymId| {
            ck.c.sym_flags(export).intersects(SymbolFlags::ALIAS)
                && ck.c.resolve_alias(export) == Some(sym)
        };
        let named = exports
            .iter()
            .find(|(name, export)| {
                (name == "default" || is_identifier_text(name)) && exports_symbol(self, *export)
            })
            .map(|(name, _)| name.clone());
        if let Some(name) = named {
            return self.module_member_entity(sym, module, &name, cx);
        }
        if !exports
            .iter()
            .any(|(name, export)| name == "export=" && exports_symbol(self, *export))
        {
            return None;
        }
        // A namespace import (`import * as y`, `import y = require()`) of the module names
        // its `export =` (`y.Argv` for a member of the namespace).
        if let Some(alias) = self.alias_in_file_for(cx.enclosing, module, meaning::NAMESPACE) {
            cx.refs.push(alias);
            let an = self.c.sym_name(alias).to_owned();
            return Some(cx.ident(&an));
        }
        let Some(spec) = self.module_specifier_for(cx.enclosing.file, module, cx.host) else {
            if let Some(path) = self.unportable_path(cx.enclosing.file, module) {
                cx.not_portable = Some((self.c.sym_name(sym).to_owned(), path));
            }
            return None;
        };
        let arg = cx.string_lit(&spec);
        Some(cx.add(Node::ImportType {
            is_typeof: false,
            arg,
            attributes: None,
            qualifier: None,
            type_args: None,
        }))
    }

    /// `import("m").name` through a module the enclosing file imports (`m`) that exports
    /// `sym` under `name` (explicitly or through `export *`), when its specifier is better
    /// than the declaring module's (`parent`): a package specifier before a relative one,
    /// then fewer path segments, then the file's import order, the declaring module last
    /// among equals (observed: `import("typescript-fsa").A` over
    /// `"typescript-fsa/src/impl"`, `import("ext").A2` over an import of `"ext/other"`).
    fn reexporting_import(
        &mut self,
        sym: SymId,
        parent: SymId,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        let file = cx.enclosing.file;
        let count = self.c.bound(file).module_requests.len();
        let named = |name: &str| name == "default" || is_identifier_text(name);
        // (specifier, export name) of each imported module that exports `sym`.
        let mut found: Vec<(String, String)> = Vec::new();
        for i in 0..count {
            let req = &self.c.bound(file).module_requests[i];
            if req.in_ambient_module {
                continue;
            }
            let spec = self.ast(file).atom(req.specifier).to_owned();
            let target = self.c.link.module_target(file, i as u32);
            let Some(m) = self.c.module_target_symbol(target) else {
                continue;
            };
            if m == parent || found.iter().any(|(s, _)| *s == spec) {
                continue;
            }
            let exports = self.c.exports_of(m);
            let explicit = exports.iter().find(|(name, id)| {
                named(name)
                    && self.c.sym_flags(*id).intersects(SymbolFlags::ALIAS)
                    && self.c.resolve_alias(*id) == Some(sym)
            });
            let any =
                explicit.or_else(|| exports.iter().find(|(name, id)| named(name) && *id == sym));
            if let Some((name, _)) = any {
                found.push((spec, name.clone()));
            }
        }
        if found.is_empty() {
            return None;
        }
        let rank = |spec: &str| {
            let relative =
                spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../");
            (relative, spec.split('/').filter(|p| *p != ".").count())
        };
        let (spec, name) = found.iter().min_by_key(|(s, _)| rank(s))?.clone();
        if let Some(own) = self.module_specifier_for(file, parent, cx.host)
            && rank(&own) <= rank(&spec)
        {
            return None;
        }
        let arg = cx.string_lit(&spec);
        let qualifier = cx.ident(&name);
        Some(cx.add(Node::ImportType {
            is_typeof: false,
            arg,
            attributes: None,
            qualifier: Some(qualifier),
            type_args: None,
        }))
    }

    /// `import("./m").name` through another module of the program that re-exports `sym`
    /// when its relative specifier has fewer path segments than the declaring module's
    /// (`..` for an `export *` index over `../../types`), or, for a package subpath, a
    /// module of the same package with fewer segments (its entry point). Observed in tsc 7:
    /// a package specifier for the declaring module wins over relative ones; a tie keeps
    /// the declaring module; among re-exporters with the same count the first in program
    /// order wins.
    fn shorter_reexport(
        &mut self,
        sym: SymId,
        parent: SymId,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        let from = cx.enclosing.file;
        // A file that imports the declaring module names it through that import.
        let count = self.c.bound(from).module_requests.len();
        for i in 0..count {
            let target = self.c.link.module_target(from, i as u32);
            if self.c.module_target_symbol(target) == Some(parent) {
                return None;
            }
        }
        // A module without a portable specifier (a package's internal chunk that its
        // `exports` hide) is named through any re-exporter (observed:
        // `import("vitest").Mock` for a chunk's `w` that `vitest` re-exports as `Mock`).
        let original = self.module_specifier_for(from, parent, cx.host);
        // A package subpath (`pkg/types/react`) gives way only to a module of the same
        // package with fewer segments that re-exports the symbol, such as its entry point
        // (observed: `import("create-emotion-styled").X` over
        // `"create-emotion-styled/types/react"`). A relative path gives way to a package
        // specifier (a symlinked package's re-exporting file:
        // `import("@scope/pkg2/dist/secondary").IdType` over `"../../pkg1/dist"`), or to a
        // relative path with fewer segments.
        let package = original
            .as_deref()
            .filter(|o| !is_relative_specifier(o))
            .map(|o| ts_r_resolver::split_package_name(o).0.to_owned());
        let segments = |s: &str| s.split('/').filter(|p| *p != ".").count();
        let rank = |s: &str| (is_relative_specifier(s), segments(s));
        let mut best: Option<(String, String)> = None;
        let mut best_rank = original.as_deref().map_or((true, usize::MAX), rank);
        for (m, name) in self.reexporters(sym) {
            if best_rank <= (false, 1) {
                break;
            }
            let Some(target) = self.c.first_decl(m).map(|d| d.file) else {
                continue;
            };
            if target == from || !(name == "default" || is_identifier_text(&name)) {
                continue;
            }
            let Some(spec) = self.module_specifier_for(from, m, cx.host) else {
                continue;
            };
            if let Some(p) = &package
                && (is_relative_specifier(&spec) || ts_r_resolver::split_package_name(&spec).0 != p)
            {
                continue;
            }
            if rank(&spec) < best_rank {
                best_rank = rank(&spec);
                best = Some((spec, name));
            }
        }
        let (spec, name) = best?;
        let arg = cx.string_lit(&spec);
        let qualifier = cx.ident(&name);
        Some(cx.add(Node::ImportType {
            is_typeof: false,
            arg,
            attributes: None,
            qualifier: Some(qualifier),
            type_args: None,
        }))
    }

    /// The modules of the program that re-export `sym` (explicitly or through `export *`)
    /// with the export name, in program order. Built once per checker.
    fn reexporters(&self, sym: SymId) -> Vec<(SymId, String)> {
        let index = self.c.cached.reexporters.get_or_init(|| {
            let mut index: rustc_hash::FxHashMap<SymId, Vec<(SymId, String)>> =
                rustc_hash::FxHashMap::default();
            for i in 0..self.c.files.len() {
                let file = FileId(i as u32);
                if self.c.file(file).is_lib {
                    continue;
                }
                let target = ts_r_binder::program::ModuleTarget::File(file);
                let Some(m) = self.c.module_target_symbol(target) else {
                    continue;
                };
                for (name, id) in self.c.exports_of(m) {
                    let resolved = if self.c.sym_flags(id).intersects(SymbolFlags::ALIAS) {
                        match self.c.resolve_alias(id) {
                            Some(r) => r,
                            None => continue,
                        }
                    } else {
                        id
                    };
                    if self.c.sym_parent(resolved) == Some(m) {
                        continue;
                    }
                    let list = index.entry(resolved).or_default();
                    if !list.iter().any(|(x, _)| *x == m) {
                        list.push((m, name));
                    }
                }
            }
            index
        });
        index.get(&sym).cloned().unwrap_or_default()
    }

    /// `typeof v` for the type of a function or arrow expression `d` initializing the
    /// variable `v` (tsc names it so everywhere but in `v`'s own declaration).
    fn typeof_initialized_variable(&mut self, d: Loc, cx: &mut TypeNodeCx) -> Option<NodeId> {
        let (v, declarator) = self.initialized_variable(d)?;
        if declarator == cx.enclosing || self.c.decls(v).contains(&cx.enclosing) {
            return None;
        }
        let expr_name = self.entity_of(v, meaning::VALUE, cx)?;
        Some(cx.add(Node::TypeQuery {
            expr_name,
            type_args: None,
        }))
    }

    /// `C.m` for a static method `m` of a class `C` that can be named.
    fn static_method_entity(&mut self, sym: SymId, cx: &mut TypeNodeCx) -> Option<NodeId> {
        let d = self.c.first_decl(sym)?;
        let ast = self.ast(d.file);
        let Node::MethodDef { modifiers, key, .. } = *ast.node(d.node) else {
            return None;
        };
        if !modifiers.contains(Modifiers::STATIC) {
            return None;
        }
        let name = crate::nodes::ident_text(ast, key)?.to_owned();
        let class = self.c.file(d.file).parents().parent(d.node)?;
        if !matches!(ast.node(class), Node::ClassDecl { .. }) {
            return None;
        }
        let class_sym = self.c.sym_of_decl(Loc::new(d.file, class))?;
        let left = self.entity_of(class_sym, meaning::VALUE, cx)?;
        let right = cx.ident(&name);
        Some(cx.add(Node::QualifiedName { left, right }))
    }

    /// The variable whose initializer is the function or arrow expression `d`.
    fn initialized_variable(&self, d: Loc) -> Option<(SymId, Loc)> {
        let ast = self.ast(d.file);
        if !matches!(
            ast.node(d.node),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        ) {
            return None;
        }
        let parents = self.c.file(d.file).parents();
        let mut p = parents.parent(d.node)?;
        while matches!(ast.node(p), Node::Paren { .. }) {
            p = parents.parent(p)?;
        }
        match *ast.node(p) {
            Node::VarDeclarator { name, .. } if matches!(ast.node(name), Node::Ident { .. }) => {
                let at = Loc::new(d.file, p);
                let v = self
                    .c
                    .sym_of_decl(at)
                    .or_else(|| self.c.sym_of_decl(Loc::new(d.file, name)))?;
                Some((v, at))
            }
            _ => None,
        }
    }

    /// For an accessor declared in a class (or an `accessor` field): its setter's parameter
    /// name (`value` without a setter, `arg` for an `accessor` field). For an object
    /// literal, only a pair whose accessors are both annotated is written as accessors.
    fn class_accessor_param(&self, p: SymId) -> Option<String> {
        // (Accessors merged from several classes, as in an intersection, are properties.)
        let mut class: Option<Loc> = None;
        let mut param = None;
        for d in self.c.decls(p) {
            let ast = self.ast(d.file);
            if let Node::PropertyDef { modifiers, .. } = *ast.node(d.node)
                && modifiers.contains(Modifiers::ACCESSOR)
            {
                return Some("arg".to_owned());
            }
            let Node::MethodDef { kind, value, .. } = *ast.node(d.node) else {
                continue;
            };
            let parent = self.c.file(d.file).parents().parent(d.node)?;
            if !matches!(
                ast.node(parent),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            ) {
                return None;
            }
            let at = Loc::new(d.file, parent);
            if class.is_some_and(|c| c != at) {
                return None;
            }
            class = Some(at);
            if kind == MethodKind::Set
                && let Node::FunctionExpr { params, .. } = *ast.node(value)
                && let Some(&first) = ast.list(params).first()
                && let Node::Param { pattern, .. } = *ast.node(first)
            {
                param = crate::nodes::ident_text(ast, pattern).map(str::to_owned);
            }
        }
        class.map(|_| param.unwrap_or_else(|| "value".to_owned()))
    }

    /// An object literal accessor pair whose getter has a return type and whose setter has
    /// an annotated parameter is written as accessors: the setter's parameter name and
    /// annotation, and the setter.
    fn object_accessor_pair(&self, p: SymId) -> Option<(String, Loc, Loc)> {
        let d = *self.c.decls(p).first()?;
        let ast = self.ast(d.file);
        let Node::MethodDef { key, .. } = *ast.node(d.node) else {
            return None;
        };
        let obj = self.c.file(d.file).parents().parent(d.node)?;
        let Node::ObjectLit { props } = *ast.node(obj) else {
            return None;
        };
        let name = crate::nodes::key_text(ast, key)?;
        let mut getter = false;
        let mut setter = None;
        for &m in ast.list(props) {
            let Node::MethodDef {
                kind, key, value, ..
            } = *ast.node(m)
            else {
                continue;
            };
            if crate::nodes::key_text(ast, key).as_deref() != Some(name.as_str()) {
                continue;
            }
            let Node::FunctionExpr {
                params,
                return_type,
                ..
            } = *ast.node(value)
            else {
                continue;
            };
            match kind {
                MethodKind::Get => getter = return_type.is_some(),
                MethodKind::Set => {
                    setter = ast.list(params).first().and_then(|&f| match *ast.node(f) {
                        Node::Param {
                            pattern,
                            type_ann: Some(t),
                            ..
                        } => Some((
                            crate::nodes::ident_text(ast, pattern)?.to_owned(),
                            Loc::new(d.file, t),
                            Loc::new(d.file, m),
                        )),
                        _ => None,
                    });
                }
                _ => {}
            }
        }
        if getter { setter } else { None }
    }

    /// The object literal member that declares property `p` (or the property it is a
    /// widened copy of): the emitter decides whether its comments are written.
    fn object_literal_member_decl(&self, p: SymId) -> Option<Loc> {
        let d = *self.c.decls(p).first()?;
        self.is_literal_member(d).then_some(d)
    }

    /// Whether `p` is declared by an `accessor` field (written as a get/set pair).
    fn is_auto_accessor(&self, p: SymId) -> bool {
        self.c.decls(p).iter().any(|d| {
            matches!(*self.ast(d.file).node(d.node),
                Node::PropertyDef { modifiers, .. } if modifiers.contains(Modifiers::ACCESSOR))
        })
    }

    /// Whether `d` is a member of an object literal or class.
    fn is_literal_member(&self, d: Loc) -> bool {
        self.c
            .file(d.file)
            .parents()
            .parent(d.node)
            .is_some_and(|parent| {
                matches!(
                    self.ast(d.file).node(parent),
                    Node::ObjectLit { .. } | Node::ClassExpr { .. } | Node::ClassDecl { .. }
                )
            })
    }

    /// The method declaration of signature `sig` when it is an object literal or class
    /// member (each overload's comments are written on its signature).
    fn method_sig_decl(&self, sig: SigId) -> Option<Loc> {
        let (f, n) = self.store().sig(sig).decl?;
        let file = ts_r_diagnostics::FileId(f);
        let mut node = NodeId::from_raw(n)?;
        if let Node::FunctionExpr { .. } = self.ast(file).node(node)
            && let Some(m) = self.c.file(file).parents().parent(node)
            && let Node::MethodDef { .. } = self.ast(file).node(m)
        {
            node = m;
        }
        let d = Loc::new(file, node);
        (matches!(self.ast(file).node(node), Node::MethodDef { .. }) && self.is_literal_member(d))
            .then_some(d)
    }

    /// How a property name is written: a quoted name as declared (`"a-b"`), a numeric-like
    /// name from a literal computed key in brackets (`["404"]`, `[5]`), from another string
    /// key quoted (`"100"` for `[k]` with `k = "100"`), a negative number in brackets
    /// (`[-1]`), else as the type printer writes it.
    fn property_key_text(&mut self, p: SymId, name: &str) -> String {
        if let Some(q) = self.declared_quoted_name(p) {
            return q;
        }
        let printed = self.property_name_for_print(name);
        let numeric = !printed.starts_with('"') && !is_identifier_text(&printed);
        if !numeric {
            return printed;
        }
        // A spread or instantiated property: the declaration it comes from.
        let decl = self
            .c
            .synth_sym(p)
            .and_then(|x| {
                x.decl
                    .or_else(|| x.origins.first().and_then(|&o| self.c.first_decl(o)))
            })
            .or_else(|| self.c.first_decl(p));
        let string_key = decl.and_then(|d| {
            let ast = self.ast(d.file);
            let key = match *ast.node(d.node) {
                Node::PropertySignature { key, .. }
                | Node::PropertyDef { key, .. }
                | Node::MethodSignature { key, .. }
                | Node::Property { key, .. } => key,
                _ => return None,
            };
            let Node::ComputedKey { expr } = *ast.node(key) else {
                return Some((matches!(ast.node(key), Node::StringLit { .. }), false));
            };
            match ast.node(expr) {
                Node::StringLit { .. } => Some((true, true)),
                Node::NumberLit { .. } => Some((false, true)),
                Node::TemplateLit { .. } => Some((true, false)),
                Node::Unary { .. } => Some((false, false)),
                _ => {
                    let t = self.check_expr(Loc::new(d.file, expr));
                    let f = self.store().flags(t);
                    Some((f.intersects(TypeFlags::STRING_LITERAL), false))
                }
            }
        });
        // A numeric-like name from a literal computed key keeps its brackets (`["404"]`,
        // `[5]`, observed).
        match string_key {
            Some((true, true)) => return format!("[{}]", crate::print::quote_string(name)),
            Some((false, true)) => return format!("[{printed}]"),
            Some((true, false)) => return crate::print::quote_string(name),
            _ => {}
        }
        if name.starts_with('-') {
            return format!("[{name}]");
        }
        printed
    }

    /// Why the key of unique symbol `sym` (named `base`) cannot be written: on a variable,
    /// a symbol of another module is TS4023 (observed); otherwise not nameable.
    fn unnameable_symbol_key(&self, sym: SymId, base: &str, cx: &TypeNodeCx) -> NotNameable {
        if cx.report_unnameable
            && let Some(d) = self.c.first_decl(sym)
            && d.file != cx.enclosing.file
            && self.c.file(d.file).bound.is_module
        {
            let path = &self.c.file(d.file).path;
            return NotNameable::CannotBeNamed {
                name: base.to_owned(),
                module: format!("\"{}\"", crate::print::strip_ts_ext(path)),
            };
        }
        NotNameable::Inaccessible(base.to_owned())
    }

    /// `[k]` for a property keyed by unique symbol `k`, named as it resolves from the
    /// enclosing file. A computed key cannot be an import type, so a symbol the file cannot
    /// name makes the type not nameable.
    fn symbol_key_text(
        &mut self,
        name: &str,
        printed: String,
        cx: &mut TypeNodeCx,
    ) -> Result<String, NotNameable> {
        let Some((base, id)) = name.strip_prefix("__@").and_then(|r| r.rsplit_once('@')) else {
            return Ok(printed);
        };
        let Some(sym) = id
            .parse::<u32>()
            .ok()
            .map(SymId)
            .filter(|&s| self.c.sym_exists(s))
        else {
            return Ok(printed);
        };
        if self
            .c
            .sym_parent(sym)
            .is_some_and(|p| self.c.sym_name(p) == "SymbolConstructor")
        {
            return Ok(printed);
        }
        let mut parts = Vec::new();
        let mut n = self.entity_of(sym, meaning::VALUE, cx);
        while let Some(id) = n {
            match *cx.out.node(id) {
                Node::Ident { name } => {
                    parts.push(cx.out.atom(name).to_owned());
                    n = None;
                }
                Node::QualifiedName { left, right } => {
                    if let Node::Ident { name } = *cx.out.node(right) {
                        parts.push(cx.out.atom(name).to_owned());
                    }
                    n = Some(left);
                }
                _ => return Err(self.unnameable_symbol_key(sym, base, cx)),
            }
        }
        if parts.is_empty() {
            return Err(self.unnameable_symbol_key(sym, base, cx));
        }
        parts.reverse();
        Ok(format!("[{}]", parts.join(".")))
    }

    /// The type of the annotation of optional property `p` when `pt` (its type) is that
    /// type plus the `undefined` of optionality: tsc writes such a property from its
    /// annotation (`once?: boolean`); an instantiated one (`x?: K` as `"x"`) keeps
    /// `| undefined`.
    fn optional_annotation_type(&mut self, p: SymId, pt: TypeId) -> Option<TypeId> {
        // A spread's copy of the property has its declaration (`prop_annotation`).
        if self.c.synth_sym(p).is_none() && self.c.decls(p).len() != 1 {
            return None;
        }
        let ann = self.prop_annotation(p)?;
        let at = self.type_from_node(ann);
        if at == pt {
            return Some(at);
        }
        let with_undefined = self.union(&[at, TypeId::UNDEFINED]);
        (with_undefined == pt).then_some(at)
    }

    /// A `readonly` parameter property (`constructor(readonly a: number)`).
    fn is_readonly_param_property(&self, p: SymId) -> bool {
        self.c.first_decl(p).is_some_and(|d| {
            matches!(*self.ast(d.file).node(d.node),
                Node::Param { modifiers, .. } if modifiers.contains(Modifiers::READONLY))
        })
    }

    /// Whether `sym` is declared at the top level of module `file` with an `export`
    /// modifier (so its local name is in scope throughout the file).
    fn is_own_export_of_file(&self, file: FileId, sym: SymId, m: u8) -> bool {
        let b = self.c.bound(file);
        let Some(parent) = self.c.sym_parent(sym) else {
            return false;
        };
        parent == self.c.sym(file, b.file_symbol)
            && !self.c.sym_flags(sym).intersects(SymbolFlags::ALIAS)
            && self.c.has_meaning(sym, m)
            && self.c.decls(sym).iter().any(|d| d.file == file)
    }

    /// The name a symbol is declared with (a default export's local name).
    fn local_name_of(&self, sym: SymId) -> Option<String> {
        let name = self.c.sym_name(sym);
        if name == "default" || name.starts_with("__") {
            let d = self.c.first_decl(sym)?;
            let ast = self.ast(d.file);
            let n = match *ast.node(d.node) {
                Node::ClassDecl { name, .. } | Node::FunctionDecl { name, .. } => name?,
                _ => return None,
            };
            return crate::nodes::ident_text(ast, n).map(str::to_owned);
        }
        if name.starts_with('"') {
            return None;
        }
        Some(name.to_owned())
    }

    fn same_symbol(&self, found: SymId, sym: SymId) -> bool {
        if found == sym {
            return true;
        }
        if self.c.sym_flags(found).intersects(SymbolFlags::ALIAS) {
            return self.c.resolve_alias(found) == Some(sym);
        }
        false
    }

    fn is_ambient_module_symbol(&self, s: SymId) -> bool {
        self.c.sym_name(s).starts_with('"')
    }

    /// A top-level import alias of the enclosing file that resolves to `sym`.
    /// An `import x = ...` alias of `sym` in the locals of a scope around `at` that is
    /// searched before `sym` is found by its own name: scopes are searched from the inside
    /// out, each by its locals (the name, then aliases) and then its container's exports.
    /// `(alias, qualified)`: `qualified` when the alias names a namespace that exports
    /// `sym` (`alias.sym`).
    fn local_alias_first(&self, at: Loc, sym: SymId, m: u8) -> Option<(SymId, bool)> {
        if !self.c.has_meaning(sym, m) {
            return None;
        }
        let b = self.c.bound(at.file);
        let ast = self.ast(at.file);
        let parents = self.c.file(at.file).parents();
        let mut cur = Some(at.node);
        let mut guard = 0;
        while let Some(node) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            if let Some(scope) = b.scope_of(node) {
                let locals = b.scopes[scope.0 as usize].locals;
                let mut alias = None;
                for &(_, local) in b.table(locals) {
                    let id = self.c.sym(at.file, local);
                    if id == sym {
                        return None;
                    }
                    if alias.is_none()
                        && self.c.sym_flags(id).intersects(SymbolFlags::ALIAS)
                        && self.resolve_name_at(at, self.c.sym_name(id), m) == Some(id)
                        && self.c.first_decl(id).is_some_and(|d| {
                            let a = self.ast(d.file);
                            let parent = self.c.file(d.file).parents().parent(d.node);
                            matches!(a.node(d.node), Node::ImportEquals { .. })
                                || parent
                                    .is_some_and(|p| matches!(a.node(p), Node::ImportEquals { .. }))
                        })
                        && let Some(target) = self.c.resolve_alias(id)
                    {
                        if target == sym {
                            alias = Some((id, false));
                        } else if self.c.sym_parent(sym) == Some(target)
                            && self.c.sym_flags(target).intersects(
                                SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE,
                            )
                            && self.c.exports_of(target).iter().any(|&(_, e)| e == sym)
                        {
                            alias = Some((id, true));
                        }
                    }
                }
                if alias.is_some() {
                    return alias;
                }
            }
            if matches!(ast.node(node), Node::Module { .. })
                && let Some(ns) = self.c.sym_of_decl(Loc::new(at.file, node))
                && self.c.exports_of(ns).iter().any(|&(_, id)| id == sym)
            {
                return None;
            }
            cur = parents.parent(node);
        }
        None
    }

    fn alias_in_file_for(&self, at: Loc, sym: SymId, m: u8) -> Option<SymId> {
        if !self.c.has_meaning(sym, m) {
            return None;
        }
        let b = self.c.bound(at.file);
        let ast = self.ast(at.file);
        let parents = self.c.file(at.file).parents();
        let file_locals = b.scopes.first().map(|s| s.locals);
        // Any alias in scope: an import declaration's or an `import x =` (the emitter
        // writes a namespace's `import x = m.c` when a name uses it).
        let mut cur = Some(at.node);
        let mut guard = 0;
        let mut seen_file = false;
        while let Some(node) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            if let Some(scope) = b.scope_of(node) {
                let locals = b.scopes[scope.0 as usize].locals;
                seen_file |= Some(locals) == file_locals;
                for &(_, local) in b.table(locals) {
                    let id = self.c.sym(at.file, local);
                    // A default import of a module without a default export names the
                    // module under `allowSyntheticDefaultImports` (observed: `http.Server`).
                    if self.c.sym_flags(id).intersects(SymbolFlags::ALIAS)
                        && self
                            .c
                            .resolve_alias(id)
                            .or_else(|| self.c.synthetic_default_module(id))
                            == Some(sym)
                        && self.resolve_name_at(at, self.c.sym_name(id), m) == Some(id)
                    {
                        return Some(id);
                    }
                }
            }
            if matches!(ast.node(node), Node::Module { .. })
                && let Some(ns) = self.c.sym_of_decl(Loc::new(at.file, node))
            {
                for (_, id) in self.c.exports_of(ns) {
                    if self.c.sym_flags(id).intersects(SymbolFlags::ALIAS)
                        && self.c.decls(id).iter().any(|d| d.file == at.file)
                        && self.c.resolve_alias(id) == Some(sym)
                        && self
                            .resolve_name_at(at, self.c.sym_name(id), m)
                            .is_none_or(|found| found == id)
                    {
                        return Some(id);
                    }
                }
            }
            cur = parents.parent(node);
        }
        // `export import x = n.c` at the top of a module is in its export table.
        if b.is_module {
            let fs = self.c.sym(at.file, b.file_symbol);
            for (_, id) in self.c.exports_of(fs) {
                if self.c.sym_flags(id).intersects(SymbolFlags::ALIAS)
                    && self.c.decls(id).iter().any(|d| d.file == at.file)
                    && matches!(
                        self.c.first_decl(id).map(|d| *ast.node(d.node)),
                        Some(Node::ImportEquals { .. })
                    )
                    && self.c.resolve_alias(id) == Some(sym)
                    && self
                        .resolve_name_at(at, self.c.sym_name(id), m)
                        .is_none_or(|found| found == id)
                {
                    return Some(id);
                }
            }
        }
        if !seen_file && let Some(locals) = file_locals {
            for &(_, local) in b.table(locals) {
                let id = self.c.sym(at.file, local);
                if self.c.sym_flags(id).intersects(SymbolFlags::ALIAS)
                    && self.c.resolve_alias(id) == Some(sym)
                    && self.resolve_name_at(at, self.c.sym_name(id), m) == Some(id)
                {
                    return Some(id);
                }
            }
        }
        None
    }

    /// `import("./m").name` for an export of a module, or `None` when no specifier for
    /// the module can be written.
    fn import_type_entity(
        &mut self,
        module: SymId,
        name: &str,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        let Some(spec) = self.module_specifier_for(cx.enclosing.file, module, cx.host) else {
            if let Some(path) = self.unportable_path(cx.enclosing.file, module) {
                cx.not_portable = Some((name.to_owned(), path));
            }
            return None;
        };
        let arg = cx.string_lit(&spec);
        let qualifier = cx.ident(name);
        Some(cx.add(Node::ImportType {
            is_typeof: false,
            arg,
            attributes: None,
            qualifier: Some(qualifier),
            type_args: None,
        }))
    }

    /// The specifier the file `from` would import `module` with: one of its own import
    /// specifiers that resolves there, else the module's package (through `host`), else a
    /// relative path (`./m`), else the package path after `node_modules`
    /// ([`specifier`] has the rules).
    pub fn module_specifier_for(
        &self,
        from: ts_r_diagnostics::FileId,
        module: SymId,
        host: Option<&dyn ModuleSpecifierHost>,
    ) -> Option<String> {
        let name = self.c.sym_name(module);
        if let Some(inner) = name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
            return Some(inner.to_owned());
        }
        // `declare module "m" { }`: its name.
        if let Some(ts_r_binder::program::ModuleTarget::Ambient(_)) =
            self.c.module_target_of(module)
        {
            return Some(name.to_owned());
        }
        let target = self.c.first_decl(module)?.file;
        let target_path = self.c.file(target).path.clone();
        let b = self.c.bound(from);
        let ast = self.ast(from);
        for req in &b.module_requests {
            let spec = ast.atom(req.specifier);
            if let Some(Some(r)) = self.c.resolution(from, spec, req)
                && r.resolved == target_path
            {
                return Some(spec.to_owned());
            }
        }
        let ff = self.c.file(from);
        let from_path = ff.path.as_str();
        let node_esm = self.c.opts.module.is_node() && ff.package_esm;
        // A file whose own relative imports name `.js` (or `.ts`) files keeps the extension.
        let relative_ending = |exts: &[&str]| {
            b.module_requests.iter().any(|req| {
                let spec = ast.atom(req.specifier);
                spec.starts_with('.') && exts.iter().any(|e| spec.ends_with(e))
            })
        };
        let js_imports = relative_ending(&[".js", ".mjs", ".cjs", ".jsx"]);
        // A `.ts` specifier of a declaration file (`./obj.ts` for `obj.d.ts`) does not
        // count (observed).
        let ts_imports = b.module_requests.iter().any(|req| {
            let spec = ast.atom(req.specifier);
            spec.starts_with('.')
                && [".ts", ".tsx", ".mts", ".cts"]
                    .iter()
                    .any(|e| spec.ends_with(e))
                && !matches!(
                    self.c.resolution(from, spec, req),
                    Some(Some(r)) if is_declaration_path(&r.resolved)
                )
        });
        let ending = if ts_imports {
            Ending::Ts
        } else if node_esm || js_imports {
            Ending::Js
        } else {
            Ending::Minimal
        };
        let module = self.c.opts.module;
        let mode = if (module.is_node() && !ff.package_esm)
            || matches!(module, ts_r_config::ModuleKind::CommonJS)
        {
            ts_r_resolver::ResolutionMode::Require
        } else {
            ts_r_resolver::ResolutionMode::Import
        };
        if let Some(host) = host {
            if let Some(spec) =
                specifier::package_specifier(host, from_path, &target_path, mode, ending)
            {
                return Some(spec);
            }
            // A package another file imports by name (a workspace package without a
            // `package.json` name, reached through a `node_modules` symlink).
            let bare = self
                .c
                .resolutions
                .iter()
                .filter(|((_, spec), _)| !spec.starts_with('.') && !spec.starts_with('/'))
                .filter_map(|((_, spec), r)| Some((spec.as_str(), r.as_ref()?.resolved.as_str())));
            let dir = |p: &str| p[..p.rfind('/').unwrap_or(0)].to_owned();
            let other_package = specifier::package_root(host, &dir(from_path))
                != specifier::package_root(host, &dir(&target_path));
            let mut candidates = if other_package {
                specifier::named_import_candidates(bare, &target_path, ending)
            } else {
                Vec::new()
            };
            candidates.sort();
            candidates.dedup();
            if let Some(spec) = candidates
                .into_iter()
                .find(|c| host.resolve(from_path, c, mode).as_deref() == Some(target_path.as_str()))
            {
                return Some(spec);
            }
        }
        if let Some(i) = target_path.rfind("/node_modules/") {
            let mut rest = &target_path[i + "/node_modules/".len()..];
            if host.is_some() {
                // The package candidates above were the paths that resolve back to the
                // module; this one does not (an `exports` map blocks it, or it goes
                // through a nested `node_modules`), so it is not portable.
                return None;
            }
            if let Some(r) = rest.strip_prefix("@types/") {
                rest = r;
            }
            let rest = crate::print::strip_ts_ext(rest);
            let rest = rest.strip_suffix("/index").unwrap_or(&rest).to_owned();
            return Some(rest);
        }
        Some(specifier::relative_module_specifier(
            from_path,
            &target_path,
            ending,
        ))
    }

    /// How TS2883 names a module that has no portable specifier: the path after the
    /// first `node_modules` when it goes through another one, else a relative path.
    /// `None` when the module is not in a `node_modules` directory.
    pub fn unportable_path(&self, from: ts_r_diagnostics::FileId, module: SymId) -> Option<String> {
        let target = self.c.first_decl(module)?.file;
        let target_path = self.c.file(target).path.clone();
        let i = target_path.find("/node_modules/")?;
        let ff = self.c.file(from);
        let ending = if self.c.opts.module.is_node() && ff.package_esm {
            Ending::Js
        } else {
            Ending::Minimal
        };
        let rest = &target_path[i + "/node_modules/".len()..];
        if rest.contains("/node_modules/") {
            let rest = crate::print::strip_ts_ext(rest);
            return Some(rest.strip_suffix("/index").unwrap_or(&rest).to_owned());
        }
        Some(specifier::relative_module_specifier(
            ff.path.as_str(),
            &target_path,
            ending,
        ))
    }

    /// An entity name from text (`A.B.C`) for error types that remember their name.
    fn entity_from_text(&mut self, text: &str, cx: &mut TypeNodeCx) -> NodeId {
        let mut parts = text.split('.');
        let first = parts.next().unwrap_or("");
        let mut n = cx.ident(first);
        for p in parts {
            let right = cx.ident(p);
            n = cx.add(Node::QualifiedName { left: n, right });
        }
        n
    }

    fn conditional_node(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        let s = self.store();
        let Some(d) = s.conditional_data(t) else {
            return Err(NotNameable::Unresolved);
        };
        if cx.stack.contains(&t) {
            return Ok(cx.kw(K::Any));
        }
        cx.stack.push(t);
        let root = *s.conditional_root_data(d.root);
        let check = self.instantiate(root.check, d.mapper);
        let ext = self.instantiate(root.extends, d.mapper);
        let (tt, ft) = match Loc::unpack(root.decl).map(|l| (l, *self.node(l))) {
            Some((
                l,
                Node::ConditionalType {
                    true_ty, false_ty, ..
                },
            )) => {
                let a = self.type_from_node(Loc::new(l.file, true_ty));
                let b = self.type_from_node(Loc::new(l.file, false_ty));
                (a, b)
            }
            _ => (TypeId::UNRESOLVED, TypeId::UNRESOLVED),
        };
        let tt = self.instantiate(tt, d.mapper);
        let ft = self.instantiate(ft, d.mapper);
        let r = (|| {
            let check = self.wrapped(check, cx, false)?;
            let infer = s.list_items(root.infer_params).to_vec();
            let saved = self.print_infer.len();
            let saved_names = cx.tp_names.len();
            for &p in &infer {
                if let Some(d) = self.store().type_param_data(p) {
                    let name = match cx.infer_names.iter().find(|(q, _)| *q == p) {
                        Some((_, n)) => n.clone(),
                        None => {
                            let n = self.fresh_type_param_name(p, d.sym, cx);
                            cx.infer_names.push((p, n.clone()));
                            n
                        }
                    };
                    cx.tp_names.push((p, name));
                }
            }
            self.print_infer.extend(infer);
            let extends = self.tn(ext, cx);
            self.print_infer.truncate(saved);
            let true_ty = extends.and_then(|e| Ok((e, self.tn(tt, cx)?)));
            cx.tp_names.truncate(saved_names);
            let (extends, true_ty) = true_ty?;
            let false_ty = self.tn(ft, cx)?;
            Ok(cx.add(Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            }))
        })();
        cx.stack.pop();
        r
    }

    fn object_node(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        let s = self.store();
        let Some(o) = s.object_of(t) else {
            return Ok(cx.add(Node::TypeLiteral {
                members: ts_r_ast::NodeList::default(),
            }));
        };
        let fn_expr = match o.kind {
            ObjectKind::Anonymous {
                decl,
                kind: anon::FUNCTION_EXPR,
                ..
            } => Loc::unpack(decl),
            ObjectKind::Literal { sym } if !sym.is_none() && self.c.sym_exists(sym) => {
                self.c.first_decl(sym)
            }
            _ => None,
        };
        if let Some(d) = fn_expr
            && let Some(n) = self.typeof_initialized_variable(d, cx)
        {
            return Ok(n);
        }
        match o.kind {
            ObjectKind::Literal { sym }
                if o.flags.contains(ts_r_types::ObjectFlags::CLASS_STATIC)
                    && sym != SymId::NONE =>
            {
                self.type_query(sym, cx)
            }
            ObjectKind::Declared {
                sym, type_params, ..
            } => {
                let params = s.list_items(type_params).to_vec();
                let local = self.local_type_params(sym);
                let shown = params[params.len() - local.len().min(params.len())..].to_vec();
                let before = cx.not_portable.is_some();
                match self.entity_of(sym, meaning::TYPE, cx) {
                    Some(n) => {
                        let mut args = Vec::with_capacity(shown.len());
                        for a in shown {
                            args.push(self.tn(a, cx)?);
                        }
                        Ok(cx.type_ref(n, &args))
                    }
                    None => match unportable_since(before, cx) {
                        Some(e) => Err(e),
                        None => self.structure_node(t, cx),
                    },
                }
            }
            ObjectKind::Reference { target, args } => {
                let args = s.list_items(args).to_vec();
                if let Some(shape) = s.tuple_shape_of(target) {
                    let shape = shape.clone();
                    return self.tuple_node(&shape, &args, cx);
                }
                let Some(ObjectKind::Declared {
                    sym, type_params, ..
                }) = s.object_of(target).map(|o| o.kind)
                else {
                    return Err(NotNameable::Unresolved);
                };
                let n_all = s.list_items(type_params).len();
                let local_n = self.local_type_params(sym).len();
                let shown: Vec<TypeId> = args[..n_all.min(args.len())]
                    .iter()
                    .skip(n_all.saturating_sub(local_n))
                    .copied()
                    .collect();
                if self.c.is_array_symbol(sym) && shown.len() == 1 {
                    let el = self.wrapped(shown[0], cx, true)?;
                    let arr = cx.add(Node::ArrayType { elem: el });
                    return Ok(if self.c.is_readonly_array_symbol(sym) {
                        cx.add(Node::TypeOperator {
                            op: TypeOperatorKind::Readonly,
                            ty: arr,
                        })
                    } else {
                        arr
                    });
                }
                let before = cx.not_portable.is_some();
                match self.entity_of(sym, meaning::TYPE, cx) {
                    Some(n) => {
                        let mut built = Vec::with_capacity(shown.len());
                        for a in shown {
                            built.push(self.tn(a, cx)?);
                        }
                        Ok(cx.type_ref(n, &built))
                    }
                    None => match unportable_since(before, cx) {
                        Some(e) => Err(e),
                        None => self.structure_node(t, cx),
                    },
                }
            }
            ObjectKind::Anonymous { decl, kind, .. } if decl.0 == u32::MAX => {
                let sym = SymId(decl.1);
                match kind {
                    anon::CLASS_STATIC | anon::NAMESPACE => {
                        let f = self.c.sym_flags(sym);
                        if f.intersects(SymbolFlags::MODULE_FILE)
                            || self.is_ambient_module_symbol(sym)
                        {
                            // `typeof ns` through a namespace import of the module.
                            if let Some(alias) =
                                self.alias_in_file_for(cx.enclosing, sym, meaning::VALUE)
                            {
                                cx.refs.push(alias);
                                let an = self.c.sym_name(alias).to_owned();
                                let expr_name = cx.ident(&an);
                                return Ok(cx.add(Node::TypeQuery {
                                    expr_name,
                                    type_args: None,
                                }));
                            }
                            let Some(spec) =
                                self.module_specifier_for(cx.enclosing.file, sym, cx.host)
                            else {
                                return Err(NotNameable::Inaccessible(
                                    self.c.sym_name(sym).to_owned(),
                                ));
                            };
                            let arg = cx.string_lit(&spec);
                            return Ok(cx.add(Node::ImportType {
                                is_typeof: true,
                                arg,
                                attributes: None,
                                qualifier: None,
                                type_args: None,
                            }));
                        }
                        match self.entity_of(sym, meaning::VALUE, cx) {
                            Some(n) => Ok(cx.add(Node::TypeQuery {
                                expr_name: n,
                                type_args: None,
                            })),
                            // A class's static side stays an object type (`{ new (): X; }`).
                            None => self.structure_node_with(t, cx, kind != anon::CLASS_STATIC),
                        }
                    }
                    anon::SYMBOL_FUNCTION => {
                        let f = self.c.sym_flags(sym);
                        let named = f.intersects(SymbolFlags::FUNCTION)
                            && !f.intersects(SymbolFlags::METHOD);
                        if named && let Some(n) = self.entity_of(sym, meaning::VALUE, cx) {
                            return Ok(cx.add(Node::TypeQuery {
                                expr_name: n,
                                type_args: None,
                            }));
                        }
                        // A static method: `typeof C.m`.
                        if f.intersects(SymbolFlags::METHOD)
                            && let Some(n) = self.static_method_entity(sym, cx)
                        {
                            return Ok(cx.add(Node::TypeQuery {
                                expr_name: n,
                                type_args: None,
                            }));
                        }
                        if let Some(d) = self.c.first_decl(sym)
                            && let Some(n) = self.typeof_initialized_variable(d, cx)
                        {
                            return Ok(n);
                        }
                        self.structure_node(t, cx)
                    }
                    _ => self.structure_node(t, cx),
                }
            }
            ObjectKind::Mapped { .. } if self.is_generic_mapped_type(t) => self.mapped_node(t, cx),
            _ => self.structure_node(t, cx),
        }
    }

    fn type_query(&mut self, sym: SymId, cx: &mut TypeNodeCx) -> Built {
        match self.entity_of(sym, meaning::VALUE, cx) {
            Some(n) => Ok(cx.add(Node::TypeQuery {
                expr_name: n,
                type_args: None,
            })),
            None => Err(NotNameable::Inaccessible(self.c.sym_name(sym).to_owned())),
        }
    }

    fn tuple_node(
        &mut self,
        shape: &ts_r_types::TupleShape,
        args: &[TypeId],
        cx: &mut TypeNodeCx,
    ) -> Built {
        // `[...T[]]` is `T[]` (tsc normalizes such a tuple to its array).
        if let [el] = &shape.elements[..]
            && el.kind == TupleElement::REST
            && el.label.is_none()
        {
            let e = self.wrapped(args.first().copied().unwrap_or(TypeId::ANY), cx, true)?;
            let arr = cx.add(Node::ArrayType { elem: e });
            return Ok(if shape.readonly {
                cx.add(Node::TypeOperator {
                    op: TypeOperatorKind::Readonly,
                    ty: arr,
                })
            } else {
                arr
            });
        }
        let mut elems = Vec::with_capacity(shape.elements.len());
        for (i, el) in shape.elements.iter().enumerate() {
            let mut a = args.get(i).copied().unwrap_or(TypeId::ANY);
            if el.kind == TupleElement::OPTIONAL && !self.c.opts.exact_optional_property_types {
                a = self.add_optionality(a);
            }
            let label = el.label.map(|l| self.store().str(l).to_owned());
            let ty = match el.kind {
                TupleElement::REST => {
                    let e = self.wrapped(a, cx, true)?;
                    cx.add(Node::ArrayType { elem: e })
                }
                TupleElement::OPTIONAL if label.is_none() => self.wrapped(a, cx, true)?,
                _ => self.tn(a, cx)?,
            };
            let item = match label {
                Some(l) => {
                    let name = cx.ident(&l);
                    cx.add(Node::NamedTupleMember {
                        rest: matches!(el.kind, TupleElement::REST | TupleElement::VARIADIC),
                        name,
                        optional: el.kind == TupleElement::OPTIONAL,
                        ty,
                    })
                }
                None => match el.kind {
                    TupleElement::OPTIONAL => cx.add(Node::OptionalType { ty }),
                    TupleElement::REST | TupleElement::VARIADIC => cx.add(Node::RestType { ty }),
                    _ => ty,
                },
            };
            elems.push(item);
        }
        let l = cx.out.add_list(&elems);
        let tuple = cx.add(Node::TupleType { elems: l });
        Ok(if shape.readonly {
            cx.add(Node::TypeOperator {
                op: TypeOperatorKind::Readonly,
                ty: tuple,
            })
        } else {
            tuple
        })
    }

    fn mapped_node(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        let Some(info) = self.mapped_info(t) else {
            return Err(NotNameable::Unresolved);
        };
        if cx.stack.contains(&t) {
            return Ok(cx.kw(K::Any));
        }
        cx.stack.push(t);
        let r = (|| {
            let kname = match self.store().type_param_data(info.k) {
                Some(d) => self.c.sym_name(d.sym).to_owned(),
                None => "K".to_owned(),
            };
            let c = self.mapped_constraint(&info);
            let constraint = self.tn(c, cx)?;
            let name = cx.ident(&kname);
            let type_param = cx.add(Node::TypeParam {
                modifiers: Modifiers::NONE,
                name,
                constraint: Some(constraint),
                default: None,
            });
            let name_type = match info.name_node {
                Some(n) => {
                    let raw = self.type_from_node(Loc::new(info.decl.file, n));
                    let it = self.instantiate(raw, info.mapper);
                    Some(self.tn(it, cx)?)
                }
                None => None,
            };
            let ty = match info.template_node {
                Some(n) => {
                    let raw = self.type_from_node(Loc::new(info.decl.file, n));
                    let it = self.instantiate(raw, info.mapper);
                    Some(self.tn(it, cx)?)
                }
                None => None,
            };
            Ok(cx.add(Node::MappedType {
                readonly: info.readonly,
                type_param,
                name_type,
                optional: info.optional,
                ty,
            }))
        })();
        cx.stack.pop();
        r
    }

    /// The error for recursive type `t` when its alias is declared, unexported, in another
    /// module (observed: TS4023 rather than `/*elided*/ any`).
    fn unnameable_alias(&self, t: TypeId, cx: &TypeNodeCx) -> Option<NotNameable> {
        let info = self.store().alias_of(t)?;
        let d = self.c.first_decl(info.sym)?;
        if d.file == cx.enclosing.file || !self.c.file(d.file).bound.is_module {
            return None;
        }
        let path = &self.c.file(d.file).path;
        Some(NotNameable::CannotBeNamed {
            name: self.c.sym_name(info.sym).to_owned(),
            module: format!("\"{}\"", crate::print::strip_ts_ext(path)),
        })
    }

    /// `{ ... }`, or a function / constructor type for a single signature.
    fn structure_node(&mut self, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        self.structure_node_with(t, cx, true)
    }

    /// The declaration an instantiated anonymous type (type literal, function type or
    /// expression, object literal) instantiates, or `None` for the declared type itself and
    /// for other types: what [`MAX_INSTANTIATION_NESTING`] counts.
    fn instantiation_key(&self, t: TypeId) -> Option<(u32, u32, u8)> {
        let o = self.store().object_of(t)?;
        match o.kind {
            ObjectKind::Anonymous { decl, kind, mapper } if mapper != MapperId::IDENTITY => {
                Some((decl.0, decl.1, kind))
            }
            ObjectKind::Literal { sym }
                if !sym.is_none() && o.flags.contains(ObjectFlags::INSTANTIATED) =>
            {
                Some((u32::MAX, sym.0, u8::MAX))
            }
            _ => None,
        }
    }

    /// `collapse`: a single call or construct signature is a function or constructor type.
    fn structure_node_with(&mut self, t: TypeId, cx: &mut TypeNodeCx, collapse: bool) -> Built {
        if cx.stack.contains(&t) {
            if cx.report_unnameable
                && let Some(e) = self.unnameable_alias(t, cx)
            {
                return Err(e);
            }
            return Ok(cx.elided());
        }
        if let Some(k) = self.instantiation_key(t) {
            let open = cx
                .stack
                .iter()
                .filter(|&&o| self.instantiation_key(o) == Some(k))
                .count();
            if open >= MAX_INSTANTIATION_NESTING {
                return Ok(cx.elided());
            }
        }
        cx.stack.push(t);
        let r = self.structure_inner(t, cx, collapse);
        cx.stack.pop();
        r
    }

    fn structure_inner(&mut self, t: TypeId, cx: &mut TypeNodeCx, collapse: bool) -> Built {
        // Keys of an object literal written from its syntax map to the literal's keys.
        let literal_keys = cx.literal_keys.take();
        let m = self.resolved_members(t);
        // A class's static side lists its synthesized `prototype`, which tsc never writes.
        let props: Vec<(ts_r_types::StrId, SymId)> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .filter(|&(n, p)| {
                !(self.c.synth_sym(p).is_some_and(|x| x.decl.is_none())
                    && self.store().str(n) == "prototype")
            })
            .collect();
        let call = m.call.to_vec();
        let construct = m.construct.to_vec();
        let index = m.index.to_vec();
        if collapse
            && props.is_empty()
            && index.is_empty()
            && call.len() == 1
            && construct.is_empty()
        {
            return self.signature_node(call[0], SigShape::Function, None, cx);
        }
        if collapse
            && props.is_empty()
            && index.is_empty()
            && call.is_empty()
            && construct.len() == 1
        {
            return self.signature_node(construct[0], SigShape::Constructor, None, cx);
        }
        let mut members = Vec::new();
        for &c in &call {
            members.push(self.signature_node(c, SigShape::Call, None, cx)?);
        }
        for &c in &construct {
            members.push(self.signature_node(c, SigShape::Construct, None, cx)?);
        }
        for i in &index {
            let key = self.tn(i.key, cx)?;
            let value = self.tn(i.value, cx)?;
            let pname = i
                .decl
                .and_then(Loc::unpack)
                .and_then(|d| {
                    let ast = self.ast(d.file);
                    let Node::IndexSignature { params, .. } = *ast.node(d.node) else {
                        return None;
                    };
                    let &p = ast.list(params).first()?;
                    let Node::Param { pattern, .. } = *ast.node(p) else {
                        return None;
                    };
                    crate::nodes::ident_text(ast, pattern).map(str::to_owned)
                })
                .unwrap_or_else(|| "x".to_owned());
            let pat = cx.ident(&pname);
            let param = cx.add(Node::Param {
                decorators: ts_r_ast::NodeList::default(),
                modifiers: Modifiers::NONE,
                rest: false,
                pattern: pat,
                optional: false,
                type_ann: Some(key),
                init: None,
            });
            let params = cx.out.add_list(&[param]);
            members.push(cx.add(Node::IndexSignature {
                modifiers: if i.readonly {
                    Modifiers::READONLY
                } else {
                    Modifiers::NONE
                },
                params,
                type_ann: Some(value),
            }));
        }
        // The first member written for the previous property and its object literal
        // declaration (for its comments).
        let mut pending: Option<(usize, Loc)> = None;
        let rest_props = props.clone();
        for (index, (n, p)) in props.into_iter().enumerate() {
            if let Some((i, d)) = pending.take()
                && let Some(&m) = members.get(i)
            {
                cx.member_decls.push((m, d));
            }
            if let Some(d) = self.object_literal_member_decl(p) {
                pending = Some((members.len(), d));
            }
            let name = self.store().str(n).to_owned();
            let origin = match self.c.synth_sym(p) {
                Some(x) if !x.origins.is_empty() => x.origins[0],
                _ => p,
            };
            if self.is_private_like(origin).is_some() {
                // tsc reports every such member (observed).
                let mut names = vec![name];
                for &(n2, p2) in rest_props.iter().skip(index + 1) {
                    let o2 = match self.c.synth_sym(p2) {
                        Some(x) if !x.origins.is_empty() => x.origins[0],
                        _ => p2,
                    };
                    if self.is_private_like(o2).is_some() {
                        names.push(self.store().str(n2).to_owned());
                    }
                }
                return Err(NotNameable::PrivateMember(names));
            }
            let key_text = self.property_key_text(p, &name);
            // A mapped type's property keyed by a unique symbol has no declaration to name
            // its key (observed: TS4118 even where the symbol can be named).
            if let Some(base) = name.strip_prefix("__@").and_then(|r| r.rsplit_once('@'))
                && self.c.synth_sym(p).is_some_and(|x| {
                    x.check & crate::symbols::check_flags::MAPPED != 0 && x.decl.is_none()
                })
                && self.c.decls(p).is_empty()
            {
                return Err(NotNameable::NonSerializableProperty(format!(
                    "[{}]",
                    base.0
                )));
            }
            let key_text = self.symbol_key_text(&name, key_text, cx)?;
            let key_origin = literal_keys.and_then(|lit| self.literal_key_span(p, lit, cx));
            if literal_keys.is_none() {
                cx.approx_len += name.len() + 2;
            }
            let pf = self.c.sym_flags(p);
            let optional = pf.intersects(SymbolFlags::OPTIONAL);
            // Accessors declared in a class stay accessors (`get x(): T;`); object literal
            // accessors are properties.
            let object_pair = self.object_accessor_pair(p);
            let accessor = match &object_pair {
                Some((param, _, _)) => Some(param.clone()),
                None if pf.intersects(SymbolFlags::GET_ACCESSOR | SymbolFlags::SET_ACCESSOR)
                    || self.is_auto_accessor(p) =>
                {
                    self.class_accessor_param(p)
                }
                None => None,
            };
            if let Some(setter_param) = accessor {
                let pt = self.type_of_symbol(p);
                let pf = if object_pair.is_some() || self.is_auto_accessor(p) {
                    SymbolFlags::GET_ACCESSOR | SymbolFlags::SET_ACCESSOR
                } else {
                    pf
                };
                if pf.intersects(SymbolFlags::GET_ACCESSOR) {
                    let ret = match self
                        .getter_annotation(p)
                        .and_then(|a| self.reuse_type_node(a, pt, cx))
                    {
                        Some(r) => r,
                        None => self.tn(pt, cx)?,
                    };
                    let key = cx.key(&key_text, key_origin);
                    let params = cx.out.add_list(&[]);
                    members.push(cx.add(Node::MethodSignature {
                        modifiers: Modifiers::NONE,
                        kind: MethodKind::Get,
                        key,
                        optional: false,
                        type_params: None,
                        params,
                        return_type: Some(ret),
                    }));
                }
                if pf.intersects(SymbolFlags::SET_ACCESSOR) {
                    let wt = match &object_pair {
                        Some((_, ann, _)) => self.type_from_node(*ann),
                        None => self.write_type_of_symbol(p),
                    };
                    let ann = match &object_pair {
                        Some((_, ann, _)) => Some(*ann),
                        None => self.setter_annotation(p),
                    };
                    let ty = match ann.and_then(|a| self.reuse_type_node(a, wt, cx)) {
                        Some(r) => r,
                        None => self.tn(wt, cx)?,
                    };
                    // The setter's name maps to the setter's key.
                    let key_origin = match (&object_pair, literal_keys) {
                        (Some((_, _, setter)), Some(lit)) => self.member_key_span(*setter, lit, cx),
                        _ => key_origin,
                    };
                    let key = cx.key(&key_text, key_origin);
                    let pattern = cx.ident(&setter_param);
                    let param = cx.add(Node::Param {
                        decorators: ts_r_ast::NodeList::default(),
                        modifiers: Modifiers::NONE,
                        rest: false,
                        pattern,
                        optional: false,
                        type_ann: Some(ty),
                        init: None,
                    });
                    let params = cx.out.add_list(&[param]);
                    members.push(cx.add(Node::MethodSignature {
                        modifiers: Modifiers::NONE,
                        kind: MethodKind::Set,
                        key,
                        optional: false,
                        type_params: None,
                        params,
                        return_type: None,
                    }));
                    if let Some((_, _, setter)) = object_pair
                        && let Some(&m) = members.last()
                    {
                        cx.member_decls.push((m, setter));
                    }
                }
                continue;
            }
            if pf.intersects(SymbolFlags::METHOD) && !self.is_readonly_symbol(p) {
                let pt = self.type_of_symbol(p);
                let pt = if optional { self.non_nullable(pt) } else { pt };
                let sigs = self.signatures_of(pt, SigKind::Call);
                if !sigs.is_empty() && self.store().flags(pt).contains(TypeFlags::OBJECT) {
                    // A method named `new` is quoted (not a construct signature).
                    let key_text = if key_text == "new" {
                        "\"new\"".to_owned()
                    } else {
                        key_text.clone()
                    };
                    let overloads = sigs.len() > 1;
                    for sgn in sigs {
                        let key = cx.key(&key_text, key_origin);
                        members.push(self.signature_node(
                            sgn,
                            SigShape::Method { key, optional },
                            None,
                            cx,
                        )?);
                        if overloads
                            && let Some(d) = self.method_sig_decl(sgn)
                            && let Some(&m) = members.last()
                        {
                            cx.member_decls.push((m, d));
                        }
                    }
                    if overloads {
                        pending = None;
                    }
                    continue;
                }
            }
            let readonly = self.is_readonly_symbol(p) || self.is_readonly_param_property(p);
            let mut pt = self.type_of_symbol(p);
            if optional
                && self.c.opts.strict_null_checks
                && self.c.opts.exact_optional_property_types
            {
                pt = self.filter_type(pt, |_, x| x != TypeId::MISSING);
                if self.store().flags(pt).contains(TypeFlags::NEVER) {
                    pt = TypeId::UNDEFINED;
                }
            } else if optional
                && self.c.opts.strict_null_checks
                && let Some(declared) = self.optional_annotation_type(p, pt)
            {
                // The declared annotation (`once?: boolean`) without the `undefined`
                // optionality adds.
                pt = declared;
            }
            // An object literal property's initializer, or a property's annotation, that
            // writes the property's type is written as in the source.
            let ty = if let Some(v) = self.literal_prop_value(p) {
                self.expr_tn(v, pt, cx)?
            } else {
                match self
                    .prop_annotation(p)
                    .and_then(|a| self.reuse_type_node(a, pt, cx))
                {
                    Some(r) => r,
                    None => self.tn(pt, cx)?,
                }
            };
            let key = cx.key(&key_text, key_origin);
            members.push(cx.add(Node::PropertySignature {
                modifiers: if readonly {
                    Modifiers::READONLY
                } else {
                    Modifiers::NONE
                },
                key,
                optional,
                type_ann: Some(ty),
            }));
        }
        if let Some((i, d)) = pending
            && let Some(&m) = members.get(i)
        {
            cx.member_decls.push((m, d));
        }
        let l = cx.out.add_list(&members);
        Ok(cx.add(Node::TypeLiteral { members: l }))
    }

    /// A signature as a function type, constructor type, call/construct signature or
    /// method signature. `ret` overrides the return type.
    pub fn signature_node(
        &mut self,
        sig: SigId,
        shape: SigShape,
        ret: Option<TypeId>,
        cx: &mut TypeNodeCx,
    ) -> Built {
        let d = self.store().sig(sig);
        let tps = self.store().list_items(d.type_params).to_vec();
        let saved = cx.tp_names.len();
        let parts = (|| {
            let type_params = self.type_params_node(&tps, cx)?;
            let params = self.sig_params(sig, cx)?;
            let return_type = match ret {
                Some(r) => self.tn(r, cx)?,
                None => self.return_node(sig, cx)?,
            };
            Ok((type_params, params, return_type))
        })();
        cx.tp_names.truncate(saved);
        let (type_params, params, return_type) = parts?;
        let params = cx.out.add_list(&params);
        let d = self.store().sig(sig);
        Ok(match shape {
            SigShape::Function if d.flags.contains(SigFlags::CONSTRUCT) => {
                cx.add(Node::ConstructorType {
                    is_abstract: d.flags.contains(SigFlags::ABSTRACT),
                    type_params,
                    params,
                    return_type,
                })
            }
            SigShape::Function => cx.add(Node::FunctionType {
                type_params,
                params,
                return_type,
            }),
            SigShape::Constructor => cx.add(Node::ConstructorType {
                is_abstract: d.flags.contains(SigFlags::ABSTRACT),
                type_params,
                params,
                return_type,
            }),
            SigShape::Call => cx.add(Node::CallSignature {
                type_params,
                params,
                return_type: Some(return_type),
            }),
            SigShape::Construct => cx.add(Node::ConstructSignature {
                type_params,
                params,
                return_type: Some(return_type),
            }),
            SigShape::Method { key, optional } => cx.add(Node::MethodSignature {
                modifiers: Modifiers::NONE,
                kind: MethodKind::Method,
                key,
                optional,
                type_params,
                params,
                return_type: Some(return_type),
            }),
        })
    }

    /// The name type parameter `tp` (declared by `sym`) is written with: its own, or
    /// `T_1`, `T_2`, ... when another type parameter of that name is in scope at the
    /// enclosing declaration or in a signature being printed (observed).
    fn fresh_type_param_name(&self, tp: TypeId, sym: SymId, cx: &TypeNodeCx) -> String {
        let base = self.c.sym_name(sym).to_owned();
        let taken = |name: &str| {
            cx.tp_names.iter().any(|(p, n)| *p != tp && n == name)
                || cx.infer_names.iter().any(|(p, n)| *p != tp && n == name)
                || self
                    .resolve_name_at(cx.enclosing, name, meaning::TYPE)
                    .is_some_and(|f| {
                        f != sym && self.c.sym_flags(f).intersects(SymbolFlags::TYPE_PARAMETER)
                    })
        };
        if !taken(&base) {
            return base;
        }
        (1..100)
            .map(|i| format!("{base}_{i}"))
            .find(|n| !taken(n))
            .unwrap_or(base)
    }

    /// `<T extends C = D, ...>` for type parameters.
    pub fn type_params_node(
        &mut self,
        tps: &[TypeId],
        cx: &mut TypeNodeCx,
    ) -> Result<Option<NodeId>, NotNameable> {
        if tps.is_empty() {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(tps.len());
        for &tp in tps {
            let Some(d) = self.store().type_param_data(tp) else {
                return Err(NotNameable::Unresolved);
            };
            let name = self.fresh_type_param_name(tp, d.sym, cx);
            cx.approx_len += name.len() + 2;
            cx.tp_names.push((tp, name.clone()));
            let n = cx.ident(&name);
            let constraint = match self.type_param_constraint(tp) {
                Some(c) => Some(self.tn(c, cx)?),
                None => None,
            };
            // Defaults are written too (observed: `<A, D extends number = 1>` for
            // `Array#flat`).
            let default = match self.type_param_default(tp) {
                Some(d) => Some(self.tn(d, cx)?),
                None => None,
            };
            out.push(cx.add(Node::TypeParam {
                modifiers: Modifiers::NONE,
                name: n,
                constraint,
                default,
            }));
        }
        let l = cx.out.add_list(&out);
        Ok(Some(cx.add(Node::TypeParams { params: l })))
    }

    fn sig_params(&mut self, sig: SigId, cx: &mut TypeNodeCx) -> Result<Vec<NodeId>, NotNameable> {
        let d = self.store().sig(sig);
        let params = d.params.to_vec();
        let this_type = d.this_type;
        let has_rest = d.has_rest();
        let min = d.min_args as usize;
        let mut out = Vec::with_capacity(params.len() + 1);
        if let Some(tt) = this_type {
            let ty = match self
                .sig_this_annotation(sig)
                .and_then(|a| self.reuse_type_node(a, tt, cx))
            {
                Some(r) => r,
                None => self.tn(tt, cx)?,
            };
            let pat = cx.ident("this");
            out.push(param_node(cx, pat, false, false, Some(ty)));
        }
        let n = params.len();
        for (i, p) in params.iter().enumerate() {
            let name = self.store().str(p.name).to_owned();
            cx.approx_len += name.len() + 2;
            let rest = has_rest && i == n - 1;
            let mut ty = p.ty;
            let optional = !rest && i >= min && p.optional;
            let decl = (p.sym != SymId::NONE)
                .then(|| self.c.first_decl(p.sym))
                .flatten()
                .map(|l| (l, *self.ast(l.file).node(l.node)));
            let question = matches!(decl, Some((_, Node::Param { optional: true, .. })));
            // `x?: T` written with an annotation without `undefined` is printed as written
            // (observed: `(options?: { a: boolean }) => ...`).
            let annotated_without_undefined = match decl {
                Some((
                    l,
                    Node::Param {
                        type_ann: Some(t), ..
                    },
                )) => !self.annotation_mentions_undefined(Loc::new(l.file, t)),
                _ => false,
            };
            if optional
                && self.c.opts.strict_null_checks
                && (!question || annotated_without_undefined)
            {
                let t2 = self.filter_type(ty, |ck, x| {
                    !ck.store().flags(x).contains(TypeFlags::UNDEFINED)
                });
                if !self.store().flags(t2).contains(TypeFlags::NEVER) {
                    ty = t2;
                }
            }
            let tnode = match self
                .param_annotation(p.sym)
                .and_then(|a| self.reuse_type_node(a, ty, cx))
            {
                Some(r) => r,
                // A `x?: T` written from its type (an instantiated signature) says
                // `| undefined`, with or without `strictNullChecks` (observed:
                // `(compareFn?: ((a: number, b: number) => number) | undefined)` for
                // `number[]#sort`).
                None if optional && question => {
                    let n = self.tn(ty, cx)?;
                    with_undefined_node(cx, n)
                }
                None => self.tn(ty, cx)?,
            };
            let pat = self.param_pattern(p.sym, &name, cx);
            out.push(param_node(cx, pat, rest, optional, Some(tnode)));
        }
        Ok(out)
    }

    /// Whether the type of annotation `t` includes `undefined` (or is `any`/`unknown`).
    fn annotation_mentions_undefined(&mut self, t: Loc) -> bool {
        let at = self.type_from_node(t);
        let f = self.store().flags(at);
        if f.intersects(
            TypeFlags::ANY | TypeFlags::UNKNOWN | TypeFlags::UNDEFINED | TypeFlags::VOID,
        ) || at == TypeId::UNRESOLVED
        {
            return true;
        }
        f.contains(TypeFlags::UNION)
            && self.store().members_of_union(at).iter().any(|&m| {
                self.store()
                    .flags(m)
                    .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID)
            })
    }

    /// The binding name of a parameter: its declared pattern when it is a destructuring
    /// pattern (printed as written, without initializers), else its name.
    fn param_pattern(&mut self, sym: SymId, name: &str, cx: &mut TypeNodeCx) -> NodeId {
        let _ = sym;
        cx.ident(name)
    }

    fn return_node(&mut self, sig: SigId, cx: &mut TypeNodeCx) -> Built {
        let pred = self.predicate_of(sig);
        let r = self.return_type_of(sig);
        if let Some(n) = self.reuse_return_annotation(sig, r, pred, cx) {
            return Ok(n);
        }
        match pred {
            Some(p) => {
                let param = match p.kind {
                    ts_r_types::PredicateKind::This | ts_r_types::PredicateKind::AssertsThis => {
                        cx.add(Node::ThisType {})
                    }
                    _ => {
                        let n = self.store().str(p.param_name).to_owned();
                        cx.ident(&n)
                    }
                };
                let asserts = matches!(
                    p.kind,
                    ts_r_types::PredicateKind::AssertsIdentifier
                        | ts_r_types::PredicateKind::AssertsThis
                );
                let ty = match p.ty {
                    Some(t) => Some(self.tn(t, cx)?),
                    None if !asserts => return self.tn(r, cx),
                    None => None,
                };
                Ok(cx.add(Node::TypePredicate { asserts, param, ty }))
            }
            None => self.tn(r, cx),
        }
    }
}

/// How a signature is written.
#[derive(Clone, Copy, Debug)]
pub enum SigShape {
    /// `(x) => r` (or `new (x) => r` for a construct signature).
    Function,
    /// `new (x) => r`.
    Constructor,
    /// `(x): r;` in a type literal.
    Call,
    /// `new (x): r;` in a type literal.
    Construct,
    /// `key?(x): r;` in a type literal.
    Method { key: NodeId, optional: bool },
}

fn param_node(
    cx: &mut TypeNodeCx,
    pattern: NodeId,
    rest: bool,
    optional: bool,
    type_ann: Option<NodeId>,
) -> NodeId {
    cx.add(Node::Param {
        decorators: ts_r_ast::NodeList::default(),
        modifiers: Modifiers::NONE,
        rest,
        pattern,
        optional,
        type_ann,
        init: None,
    })
}

fn number_node(cx: &mut TypeNodeCx, v: f64) -> NodeId {
    if v < 0.0 || (v == 0.0 && v.is_sign_negative()) {
        let arg = cx.add(Node::NumberLit {
            value: ts_r_ast::F64Bits::new(-v),
        });
        return cx.add(Node::Unary {
            op: ts_r_ast::UnaryOp::Minus,
            arg,
        });
    }
    cx.add(Node::NumberLit {
        value: ts_r_ast::F64Bits::new(v),
    })
}

fn bigint_node(cx: &mut TypeNodeCx, digits: &str) -> NodeId {
    if let Some(pos) = digits.strip_prefix('-') {
        let a = cx.out.intern(pos);
        let arg = cx.add(Node::BigIntLit { value: a });
        return cx.add(Node::Unary {
            op: ts_r_ast::UnaryOp::Minus,
            arg,
        });
    }
    let a = cx.out.intern(digits);
    cx.add(Node::BigIntLit { value: a })
}

/// Whether a built node needs parentheses as an array element or intersection member
/// (`wrap_union`) or a conditional's check type.
fn needs_parens(ast: &Ast, n: NodeId, wrap_union: bool) -> bool {
    match ast.node(n) {
        Node::FunctionType { .. } | Node::ConstructorType { .. } => true,
        Node::UnionType { .. } | Node::IntersectionType { .. } => wrap_union,
        Node::ConditionalType { .. } => wrap_union,
        Node::TypeOperator {
            op: TypeOperatorKind::KeyOf,
            ..
        } => wrap_union,
        Node::InferType { .. } => wrap_union,
        _ => false,
    }
}

/// Whether a built node needs parentheses as a union member.
/// What a type node built from a type adds to [`TypeNodeCx::approx_len`]: a keyword or
/// literal its text, a reference its name, a composite type its separators, anything else
/// 2 (its parts count themselves).
fn leaf_len(ast: &Ast, n: NodeId) -> usize {
    let seps = |l: ts_r_ast::NodeList, each: usize| ast.list(l).len().saturating_sub(1) * each;
    let args = |a: Option<NodeId>| match a.map(|a| ast.node(a)) {
        Some(&Node::TypeArgs { args }) => 2 + seps(args, 2),
        _ => 0,
    };
    match *ast.node(n) {
        Node::KeywordType { kind } => keyword_len(kind),
        Node::TypeRef { name, type_args } => name_len(ast, name) + args(type_args),
        Node::UnionType { types } | Node::IntersectionType { types } => seps(types, 3),
        Node::FunctionType { params, .. } => 6 + seps(params, 2),
        Node::ConstructorType { params, .. } => 10 + seps(params, 2),
        Node::TupleType { elems } => 2 + seps(elems, 2),
        Node::ConditionalType { .. } => 14,
        Node::MappedType { .. } => 12,
        Node::StringLit { value } => ast.atom(value).len() + 2,
        // `import("m").Q`: the specifier and qualifier are not type nodes of their own.
        Node::ImportType {
            arg,
            qualifier,
            type_args,
            ..
        } => {
            let spec = match *ast.node(arg) {
                Node::StringLit { value } => ast.atom(value).len() + 2,
                _ => 2,
            };
            8 + spec + qualifier.map_or(0, |q| 1 + name_len(ast, q)) + args(type_args)
        }
        _ => 2,
    }
}

/// [`leaf_len`] summed over a reused type node and its descendants (names inside a
/// reference count once, through the reference).
fn subtree_len(ast: &Ast, n: NodeId) -> usize {
    use ts_r_ast::visit::{Visitor, Walk, walk};
    struct Sum(usize);
    impl Visitor for Sum {
        fn enter(&mut self, ast: &Ast, n: NodeId) -> Walk {
            match ast.node(n) {
                Node::Ident { .. } | Node::QualifiedName { .. } => Walk::SkipChildren,
                _ => {
                    self.0 += leaf_len(ast, n);
                    Walk::Continue
                }
            }
        }
    }
    let mut s = Sum(0);
    walk(ast, n, &mut s);
    s.0
}

fn name_len(ast: &Ast, n: NodeId) -> usize {
    match *ast.node(n) {
        Node::Ident { name } => ast.atom(name).len(),
        Node::QualifiedName { left, right } => name_len(ast, left) + 1 + name_len(ast, right),
        _ => 2,
    }
}

fn keyword_len(kind: K) -> usize {
    match kind {
        K::Any => 3,
        K::Never | K::Null | K::Void => 4,
        K::Number | K::String | K::Object | K::Symbol | K::BigInt => 6,
        K::Boolean | K::Unknown => 7,
        K::Undefined | K::Intrinsic => 9,
        _ => 6,
    }
}

/// `n | undefined`: `n` as it is when it already says `undefined` or is `any`/`unknown`,
/// a union gets `undefined` appended, and a function, constructor, intersection or
/// conditional type is parenthesized first.
fn with_undefined_node(cx: &mut TypeNodeCx, n: NodeId) -> NodeId {
    let is_kw = |cx: &TypeNodeCx, n: NodeId, kinds: &[K]| matches!(cx.out.node(n), Node::KeywordType { kind } if kinds.contains(kind));
    if is_kw(cx, n, &[K::Any, K::Unknown, K::Undefined]) {
        return n;
    }
    let mut parts: Vec<NodeId> = match *cx.out.node(n) {
        Node::UnionType { types } => cx.out.list(types).to_vec(),
        _ if needs_parens_in_union(cx.out, n) => vec![cx.paren(n)],
        _ => vec![n],
    };
    if parts
        .iter()
        .any(|&p| is_kw(cx, p, &[K::Any, K::Unknown, K::Undefined]))
    {
        return n;
    }
    parts.push(cx.kw(K::Undefined));
    cx.approx_len += 12;
    let l = cx.out.add_list(&parts);
    cx.add(Node::UnionType { types: l })
}

fn needs_parens_in_union(ast: &Ast, n: NodeId) -> bool {
    matches!(
        ast.node(n),
        Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::IntersectionType { .. }
            | Node::ConditionalType { .. }
    )
}

/// `.`, `..`, `./x` or `../x` (`.prisma/client` is a package name).
fn is_relative_specifier(spec: &str) -> bool {
    spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../")
}

/// Whether `path` names a declaration file (`x.d.ts`, `x.d.mts`, `x.d.cts`).
fn is_declaration_path(path: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|e| path.ends_with(e))
}
