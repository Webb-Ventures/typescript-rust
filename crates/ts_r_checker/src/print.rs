//! Type to string, following tsc 7.0.2's printed forms (observed): unions with `null` and
//! `undefined` last and `false | true` collapsed to `boolean`, full enum literal sets
//! collapsed to the enum name, arrays as `T[]`, object literal types as
//! `{ a: number; m(): void; }`, single-signature types as arrow types.

use ts_r_binder::{SymbolFlags, meaning};
use ts_r_types::{
    ObjectFlags, ObjectKind, SigFlags, SigId, SymId, TupleElement, TypeFlags, TypeId, TypeKind,
    TypeTraits,
};

use crate::checker::{Checker, Ck, Loc};
use crate::declared::anon;
use crate::members::SigKind;
use ts_r_ast::Node;

/// How editor features print types; [`PrintMode::default`] prints as diagnostics do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrintMode {
    /// Names carry the namespaces around them, except those whose body encloses this node
    /// (as [`Ck::type_to_string_at`]).
    pub scope: Option<Loc>,
    /// A `?` parameter or property prints without the `undefined` its optionality adds
    /// (`c?: () => string`, as tsc's quick info shows it); an annotation that spells out
    /// `undefined` keeps it.
    pub omit_optional_undefined: bool,
    /// With `scope`, a type that is not in scope by its own name is named through an
    /// import alias in scope that reaches it or one of its namespaces (`a<a<string>>` for
    /// `import a = require("./a")` of `export = Foo`; `d.C<number>` for `import d = M.N`).
    pub via_aliases: bool,
    /// With `scope`, a type literal's member, a signature's parameter or its return type
    /// whose declaration's annotation writes exactly its type prints as annotated (tsc 7
    /// quick info reuses those nodes: `callback: (x: 'hi') => number`, `p: 'a' | number`,
    /// `f: { (x: number): number; }`), when every name in it means the same at `scope`.
    pub reuse_annotations: bool,
    /// The outermost type prints structurally even when it carries an alias, its own or
    /// another (`type B = A` shows `"a" | "b"`, `type Less = Exclude<..>` shows `"_1"`), as
    /// tsc's quick info prints a type alias's declared type (TypeFormatFlags.InTypeAlias).
    /// Nested types still print through their aliases.
    pub in_type_alias: bool,
}

impl PrintMode {
    /// Qualified as seen from `scope`, otherwise as diagnostics print.
    pub fn at(scope: Loc) -> Self {
        PrintMode {
            scope: Some(scope),
            ..PrintMode::default()
        }
    }

    /// tsc's quick info: as seen from `scope`, through import aliases, and `?` members
    /// without the `undefined` optionality adds.
    pub fn quick_info(scope: Loc) -> Self {
        PrintMode {
            scope: Some(scope),
            omit_optional_undefined: true,
            via_aliases: true,
            reuse_annotations: true,
            in_type_alias: false,
        }
    }

    /// [`Self::quick_info`] for a type alias's declared type: the outermost alias is
    /// expanded.
    pub fn quick_info_alias(scope: Loc) -> Self {
        PrintMode {
            in_type_alias: true,
            ..PrintMode::quick_info(scope)
        }
    }
}

const MAX_PRINT_DEPTH: u32 = 100;
/// Types one outermost print may visit before the rest is elided.
const MAX_PRINT_NODES: u32 = 100_000;

impl<'a> Checker<'a> {
    /// `Array<T>` / `ReadonlyArray<T>` references.
    pub fn is_array_type(&self, t: TypeId) -> bool {
        let s = &self.store;
        match s.object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, .. }) => match s.object_of(target).map(|o| o.kind)
            {
                Some(ObjectKind::Declared { sym, .. }) => self.is_array_symbol(sym),
                _ => false,
            },
            _ => false,
        }
    }

    pub fn is_array_symbol(&self, sym: SymId) -> bool {
        self.array_syms.contains(&Some(sym))
    }

    pub fn is_readonly_array_symbol(&self, sym: SymId) -> bool {
        self.array_syms[1] == Some(sym)
    }

    /// Arrays and tuples.
    pub fn is_array_like_type(&self, t: TypeId) -> bool {
        self.is_array_type(t) || self.store.tuple_shape_of(t).is_some()
    }

    pub fn is_tuple_type(&self, t: TypeId) -> bool {
        self.store.tuple_shape_of(t).is_some()
    }

    /// Element types of a tuple type.
    pub fn type_args_of_tuple(&self, t: TypeId) -> &[TypeId] {
        match self.store.object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { args, .. }) => self.store.list_items(args),
            _ => &[],
        }
    }
}

/// Escapes the text of a template literal type for printing.
pub fn escape_template_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes a string literal type for printing (double quotes, JSON-like).
pub fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '\u{85}' => out.push_str("\\u0085"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

impl<'c, 'a> Ck<'c, 'a> {
    /// For `sym`, a member of a global interface `I`, the global variable declared as an
    /// `I` that reaches it (`Symbol` for `SymbolConstructor.toPrimitive`): tsc names a
    /// unique symbol through the value that holds it (`typeof Symbol.toPrimitive`).
    fn interface_member_holder(&mut self, sym: SymId) -> Option<String> {
        let parent = self.c.sym_parent(sym)?;
        // An interface that is also a value (`interface I` + `declare var I: I`) is its
        // own container: tsc prints `I["key"]` then.
        if !self.c.sym_flags(parent).intersects(SymbolFlags::INTERFACE)
            || self.c.sym_flags(parent).intersects(SymbolFlags::VALUE)
        {
            return None;
        }
        let iname = self.c.sym_name(parent).to_owned();
        let declared = self.declared_type_of_symbol(parent);
        if self
            .c
            .global_symbol(&iname, meaning::TYPE)
            .is_none_or(|g| self.declared_type_of_symbol(g) != declared)
        {
            return None;
        }
        let names: Vec<Box<str>> = self.c.link.globals.iter().map(|(n, _)| n.clone()).collect();
        for n in names {
            let Some(g) = self.c.global_symbol(&n, meaning::VALUE) else {
                continue;
            };
            if !self.c.sym_flags(g).intersects(SymbolFlags::VARIABLE) {
                continue;
            }
            let decls: Vec<Loc> = self.c.decls(g).to_vec();
            let annotated = decls.iter().any(|&d| {
                let ast = self.ast(d.file);
                let d = match ast.node(d.node) {
                    Node::Ident { .. } => self.c.parent(d).unwrap_or(d),
                    _ => d,
                };
                match *ast.node(d.node) {
                    Node::VarDeclarator {
                        type_ann: Some(ta), ..
                    } => matches!(*ast.node(ta), Node::TypeRef { name, type_args: None }
                        if crate::nodes::ident_text(ast, name) == Some(iname.as_str())),
                    _ => false,
                }
            });
            if annotated {
                return Some(n.into());
            }
        }
        None
    }

    /// The printed form of a type.
    pub fn type_to_string(&mut self, t: TypeId) -> String {
        let mut stack = Vec::new();
        let s = self.print_type(t, 0, &mut stack);
        truncate(s)
    }

    /// The printed form of a type as seen from `scope` (for editor hovers): names carry the
    /// namespaces around them (`m1.m2.c`, `N.D2<number>`, `typeof M.C`), except those whose
    /// body encloses `scope`.
    pub fn type_to_string_at(&mut self, t: TypeId, scope: Loc) -> String {
        let saved = (self.print_qualified, self.print_scope);
        self.print_qualified = true;
        self.print_scope = Some(scope);
        let s = self.type_to_string(t);
        (self.print_qualified, self.print_scope) = saved;
        s
    }

    /// Runs `f` with the printer set up for `mode`, then restores the previous setup.
    fn with_print_mode<R>(&mut self, mode: PrintMode, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = (
            self.print_qualified,
            self.print_scope,
            self.print_omit_optional_undefined,
            self.print_via_aliases,
            self.print_reuse,
            self.print_skip_alias,
        );
        self.print_skip_alias = mode.in_type_alias;
        if let Some(scope) = mode.scope {
            self.print_qualified = true;
            self.print_scope = Some(scope);
            self.print_via_aliases = mode.via_aliases;
            self.print_reuse = mode.reuse_annotations;
        }
        self.print_omit_optional_undefined = mode.omit_optional_undefined;
        let r = f(self);
        (
            self.print_qualified,
            self.print_scope,
            self.print_omit_optional_undefined,
            self.print_via_aliases,
            self.print_reuse,
            self.print_skip_alias,
        ) = saved;
        r
    }

    /// The printed form of a type in an editor mode (see [`PrintMode`]).
    pub fn type_to_string_with(&mut self, t: TypeId, mode: PrintMode) -> String {
        self.with_print_mode(mode, |ck| ck.type_to_string(t))
    }

    /// A signature as seen from `scope`: names are qualified as
    /// [`Self::type_to_string_at`] qualifies them (`iterator?: Underscore.Iterator<number,
    /// boolean>`). `arrow` prints `(...) => R`, else `(...): R`.
    pub fn signature_to_string_at(
        &mut self,
        sig: SigId,
        kind: SigKind,
        arrow: bool,
        scope: Loc,
    ) -> String {
        self.signature_to_string_with(sig, kind, arrow, PrintMode::at(scope))
    }

    /// A signature in an editor mode (see [`PrintMode`]).
    pub fn signature_to_string_with(
        &mut self,
        sig: SigId,
        kind: SigKind,
        arrow: bool,
        mode: PrintMode,
    ) -> String {
        self.with_print_mode(mode, |ck| {
            let mut stack = Vec::new();
            let s = ck.print_signature(sig, kind, arrow, 0, &mut stack);
            truncate(s)
        })
    }

    /// Prints `t` before a postfix (`[]`, `?`, `[K]`): like [`Self::print_wrapped`], and a
    /// readonly array or tuple is parenthesized too (`(readonly [...T])[]`).
    fn print_postfix_operand(&mut self, t: TypeId, depth: u32, stack: &mut Vec<TypeId>) -> String {
        let s = self.print_wrapped(t, depth, stack, true);
        if s.starts_with("readonly ") {
            format!("({s})")
        } else {
            s
        }
    }

    /// Prints `t` in a position that needs parentheses around function, union and
    /// intersection types (array element types, union members).
    fn print_wrapped(
        &mut self,
        t: TypeId,
        depth: u32,
        stack: &mut Vec<TypeId>,
        wrap_union: bool,
    ) -> String {
        let s = self.print_type(t, depth, stack);
        if self.c.instantiation_query_name(t).is_some() {
            return s;
        }
        let st = self.store();
        let f = st.flags(t);
        let needs = (wrap_union
            && f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION)
            && st.alias_of(t).is_none()
            && t != TypeId::BOOLEAN
            && !st.traits(t).contains(TypeTraits::ENUM_UNION))
            || (self.prints_as_arrow(t) && st.alias_of(t).is_none())
            || (wrap_union && f.contains(TypeFlags::INDEX))
            // `(infer U)[]`
            || (wrap_union
                && f.contains(TypeFlags::TYPE_PARAMETER)
                && self.print_infer.contains(&t))
            // `(T extends U ? X : Y)[]`
            || (wrap_union && f.contains(TypeFlags::CONDITIONAL) && st.alias_of(t).is_none());
        if needs && !(f.contains(TypeFlags::UNION) && self.union_prints_single(t)) {
            format!("({s})")
        } else {
            s
        }
    }

    fn union_prints_single(&mut self, t: TypeId) -> bool {
        // A union whose printed form is one name (an enum collapsed to its name).
        let members = self.store().members_of_union(t).to_vec();
        if let Some(e) = self.enum_of_literals(&members) {
            let all = self.enum_literal_set(e);
            return all.len() == members.len();
        }
        false
    }

    fn prints_as_arrow(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let Some(o) = s.object_of(t) else {
            return false;
        };
        if !matches!(
            o.kind,
            ObjectKind::Anonymous { .. } | ObjectKind::Literal { .. }
        ) {
            return false;
        }
        if let ObjectKind::Anonymous { kind, decl, .. } = o.kind
            && decl.0 == u32::MAX
            && (kind == anon::CLASS_STATIC || kind == anon::NAMESPACE)
        {
            return false;
        }
        if let ObjectKind::Anonymous { kind, decl, .. } = o.kind
            && decl.0 == u32::MAX
            && kind == anon::SYMBOL_FUNCTION
            && self
                .c
                .sym_flags(ts_r_types::SymId(decl.1))
                .intersects(SymbolFlags::NAMESPACE)
        {
            return false;
        }
        let m = self.resolved_members(t);
        m.props.is_empty() && m.index.is_empty() && (m.call.len() + m.construct.len()) == 1
    }

    fn print_type(&mut self, t: TypeId, depth: u32, stack: &mut Vec<TypeId>) -> String {
        let mut skip_alias = false;
        let mut no_reduction = false;
        if depth == 0 && stack.is_empty() {
            self.print_nodes = 0;
            skip_alias = std::mem::take(&mut self.print_skip_alias);
            no_reduction = std::mem::take(&mut self.print_no_reduction);
        }
        self.print_nodes += 1;
        if self.print_nodes > MAX_PRINT_NODES {
            return self.elided().into();
        }
        if let Some(name) = self.c.instantiation_query_name(t) {
            return name.to_owned();
        }
        let s = self.store();
        match t {
            TypeId::ERROR
            | TypeId::UNRESOLVED
            | TypeId::IMPLICIT_ANY
            | TypeId::AUTO
            | TypeId::WILDCARD => {
                return "any".into();
            }
            TypeId::MISSING => return "undefined".into(),
            TypeId::EMPTY_OBJECT | TypeId::EMPTY_FRESH_OBJECT => return "{}".into(),
            _ => {}
        }
        if Some(t) == self.global_this {
            return "typeof globalThis".into();
        }
        if let Some(name) = s.error_name(t) {
            return name.to_owned();
        }
        // An intersection emptied by conflicting discriminants prints as `never`, also
        // through its alias (tsc reduces every type it prints).
        if !no_reduction
            && s.flags(t).contains(TypeFlags::INTERSECTION)
            && self.reduced_type(t) == TypeId::NEVER
        {
            return "never".into();
        }
        let s = self.store();
        // A forward reference prints as its alias (`type T = [number, T[]]`).
        if let Some(sym) = s.forward_alias(t).filter(|_| !skip_alias) {
            let name = self.c.sym_name(sym).to_owned();
            return self.qualified_name(sym, name);
        }
        // Aliases print by name.
        if let Some(info) = s.alias_of(t).filter(|_| !skip_alias) {
            let name = self.c.sym_name(info.sym).to_owned();
            let args = s.list_items(info.args).to_vec();
            if args.is_empty() {
                return self.qualified_name(info.sym, name);
            }
            let parts: Vec<String> = args
                .iter()
                .map(|&a| self.print_type(a, depth + 1, stack))
                .collect();
            return format!(
                "{}<{}>",
                self.qualified_name(info.sym, name),
                parts.join(", ")
            );
        }
        let f = s.flags(t);
        match s.kind(t) {
            TypeKind::Intrinsic => {
                if f.contains(TypeFlags::ANY) {
                    return "any".into();
                }
                if f.contains(TypeFlags::UNKNOWN) {
                    return "unknown".into();
                }
                if f.contains(TypeFlags::NEVER) {
                    return "never".into();
                }
                if f.contains(TypeFlags::VOID) {
                    return "void".into();
                }
                if f.contains(TypeFlags::UNDEFINED) {
                    return "undefined".into();
                }
                if f.contains(TypeFlags::NULL) {
                    return "null".into();
                }
                if f.contains(TypeFlags::STRING) {
                    return "string".into();
                }
                if f.contains(TypeFlags::NUMBER) {
                    return "number".into();
                }
                if f.contains(TypeFlags::BIGINT) {
                    return "bigint".into();
                }
                if f.contains(TypeFlags::ES_SYMBOL) {
                    return "symbol".into();
                }
                if f.contains(TypeFlags::NON_PRIMITIVE) {
                    return "object".into();
                }
                if f.contains(TypeFlags::BOOLEAN_LITERAL) {
                    return if s.regular(t) == TypeId::TRUE {
                        "true".into()
                    } else {
                        "false".into()
                    };
                }
                "any".into()
            }
            TypeKind::StringLiteral(x) => quote_string(s.str(x)),
            TypeKind::NumberLiteral(..) => {
                let v = s.number_value(t).unwrap_or(0.0);
                ts_r_binder::js_number_to_string(v)
            }
            TypeKind::BigIntLiteral(x) => format!("{}n", s.str(x)),
            TypeKind::EnumLiteral { member, .. } => {
                let parent = self.c.sym_parent(member);
                let mname = self.c.sym_name(member).to_owned();
                match parent {
                    Some(p) => {
                        let pn = self.c.sym_name(p).to_owned();
                        let q = self.qualified_name(p, pn);
                        // The only member of an enum is the enum type itself.
                        let members = self
                            .c
                            .exports_of(p)
                            .into_iter()
                            .filter(|(_, m)| {
                                self.c.sym_flags(*m).contains(SymbolFlags::ENUM_MEMBER)
                            })
                            .count();
                        if members == 1 {
                            q
                        } else if is_identifier_name(&mname) {
                            format!("{q}.{mname}")
                        } else {
                            format!("{q}[{}]", quote_string(&mname))
                        }
                    }
                    None => mname,
                }
            }
            TypeKind::Enum(sym) => {
                let n = self.c.sym_name(sym).to_owned();
                self.qualified_name(sym, n)
            }
            // A unique symbol declared as an interface member prints through the global
            // variable holding that interface, else as an indexed access on the interface
            // (`typeof Symbol.iterator`, `Holder["tag"]`; observed tsc 7).
            TypeKind::UniqueSymbol(sym) => {
                let name = self.c.sym_name(sym).to_owned();
                match self.interface_member_holder(sym) {
                    Some(v) => format!("typeof {v}.{name}"),
                    None => match self.c.sym_parent(sym) {
                        Some(p) if self.c.sym_flags(p).intersects(SymbolFlags::INTERFACE) => {
                            let iname = self.c.sym_name(p).to_owned();
                            let iname = self.qualified_name(p, iname);
                            format!("{iname}[\"{name}\"]")
                        }
                        _ => format!("typeof {name}"),
                    },
                }
            }
            TypeKind::TypeParameter(_) => {
                let d = s.type_param_data(t);
                match d {
                    Some(d) if d.is_this => "this".into(),
                    Some(d) if self.print_infer.contains(&t) => {
                        let name = self.c.sym_name(d.sym).to_owned();
                        match self.type_param_constraint(t) {
                            Some(c) => {
                                let cs = self.print_type(c, depth + 1, stack);
                                format!("infer {name} extends {cs}")
                            }
                            None => format!("infer {name}"),
                        }
                    }
                    Some(d) => self.c.sym_name(d.sym).to_owned(),
                    None => "T".into(),
                }
            }
            TypeKind::Union { list, .. } => {
                // A union that remembers how it was written prints its origin
                // (`HeadersInit | undefined`).
                let members = match s.union_origin(t) {
                    // `T & (0 | 1)` distributed over its union, written as one intersection.
                    Some(&[one]) => return self.print_type(one, depth, stack),
                    Some(o) => o.to_vec(),
                    None => s.list_items(list).to_vec(),
                };
                self.print_union(&members, depth, stack)
            }
            TypeKind::Intersection { list, .. } => {
                let members = s.list_items(list).to_vec();
                let parts: Vec<String> = members
                    .iter()
                    .map(|&m| self.print_wrapped(m, depth + 1, stack, true))
                    .collect();
                parts.join(" & ")
            }
            TypeKind::Index(x) => {
                format!("keyof {}", self.print_wrapped(x, depth + 1, stack, true))
            }
            TypeKind::IndexedAccess { obj, index } => {
                format!(
                    "{}[{}]",
                    self.print_postfix_operand(obj, depth + 1, stack),
                    self.print_type(index, depth + 1, stack)
                )
            }
            TypeKind::Substitution { base, .. } => self.print_type(base, depth, stack),
            TypeKind::StringMapping { sym, ty } => {
                format!(
                    "{}<{}>",
                    self.c.sym_name(sym),
                    self.print_type(ty, depth + 1, stack)
                )
            }
            TypeKind::Conditional(_) => self.print_conditional(t, depth, stack),
            // An unlinked forward reference (a linked one reads as its target).
            TypeKind::Forward(_) => "any".into(),
            TypeKind::TemplateLiteral(_) => {
                let Some((texts, types)) = s.template_parts(t) else {
                    return "string".into();
                };
                let texts: Vec<String> = texts
                    .iter()
                    .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                    .collect();
                let types = types.to_vec();
                let mut out = String::from("`");
                out.push_str(&escape_template_text(&texts[0]));
                for (i, &ty) in types.iter().enumerate() {
                    out.push_str("${");
                    out.push_str(&self.print_type(ty, depth + 1, stack));
                    out.push('}');
                    out.push_str(&escape_template_text(
                        texts.get(i + 1).map_or("", String::as_str),
                    ));
                }
                out.push('`');
                out
            }
            TypeKind::Object(_) => self.print_object(t, depth, stack),
        }
    }

    /// The enum all `members` are literals of (when they are all enum literals of one enum).
    pub(crate) fn enum_of_literals(&self, members: &[TypeId]) -> Option<SymId> {
        let s = self.store();
        let mut e = None;
        for &m in members {
            let TypeKind::EnumLiteral { member, .. } = s.kind(m) else {
                return None;
            };
            let p = self.c.sym_parent(member)?;
            if e.is_some_and(|x| x != p) {
                return None;
            }
            e = Some(p);
        }
        e
    }

    fn enum_literal_set(&mut self, e: SymId) -> Vec<TypeId> {
        let d = self.declared_type_of_symbol(e);
        let s = self.store();
        let members: Vec<TypeId> = if s.flags(d).contains(TypeFlags::UNION) {
            s.members_of_union(d)
                .iter()
                .map(|&m| s.regular(m))
                .collect()
        } else {
            vec![s.regular(d)]
        };
        members
    }

    fn print_union(&mut self, members: &[TypeId], depth: u32, stack: &mut Vec<TypeId>) -> String {
        let s = self.store();
        let mut parts: Vec<String> = Vec::new();
        let mut tail: Vec<&'static str> = Vec::new();
        let mut has_null = false;
        let mut has_undefined = false;
        let has_false = members.iter().any(|&m| s.regular(m) == TypeId::FALSE);
        let has_true = members.iter().any(|&m| s.regular(m) == TypeId::TRUE);
        let collapse_bool = has_false && has_true;
        // Enum literal groups that cover a whole enum collapse to the enum name, printed after
        // the other members.
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
            let all = self.enum_literal_set(*e);
            if all.len() > 1 && all.iter().all(|x| lits.contains(x)) {
                collapsed.push(*e);
            }
        }
        let mut enum_names: Vec<String> = Vec::new();
        let mut printed_enums: Vec<SymId> = Vec::new();
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
                    parts.push("boolean".into());
                }
                continue;
            }
            if let TypeKind::EnumLiteral { member, .. } = s.kind(m)
                && let Some(p) = self.c.sym_parent(member)
                && collapsed.contains(&p)
            {
                if !printed_enums.contains(&p) {
                    printed_enums.push(p);
                    let n = self.c.sym_name(p).to_owned();
                    enum_names.push(self.qualified_name(p, n));
                }
                continue;
            }
            let mut p = self.print_wrapped(m, depth + 1, stack, false);
            // `object | (object & { x: 1; })`: intersections and conditionals are
            // parenthesized in unions.
            if f.intersects(TypeFlags::INTERSECTION | TypeFlags::CONDITIONAL)
                && s.alias_of(m).is_none()
            {
                p = format!("({p})");
            }
            parts.push(p);
        }
        parts.extend(enum_names);
        if has_null {
            tail.push("null");
        }
        if has_undefined {
            tail.push("undefined");
        }
        // tsc prints `undefined` before `null`? Observed: `string | null | undefined`.
        for t in tail {
            parts.push(t.into());
        }
        // Members that would print alike (`Foo.Yep | Bar.Yep`) are qualified.
        if !self.print_qualified
            && parts
                .iter()
                .enumerate()
                .any(|(i, p)| parts[..i].contains(p))
        {
            self.print_qualified = true;
            let q = self.print_union(members, depth, stack);
            self.print_qualified = false;
            return q;
        }
        parts.join(" | ")
    }

    /// A symbol's name as printed: a class expression (`__class`) takes the name of the
    /// variable it initializes (`const A = class {}` prints `A`), else `(Anonymous class)`.
    pub(crate) fn sym_display_name(&self, sym: SymId) -> String {
        let name = self.c.sym_name(sym);
        // The export symbol of `export default class C` prints as the declaration names it
        // (`C`), except after a module path (`import("./c").default`).
        if name == "default"
            && !(self.print_qualified && self.print_module_qualified)
            && let Some(d) = self.c.first_decl(sym)
        {
            let ast = self.c.ast(d.file);
            let named = match *ast.node(d.node) {
                Node::ClassDecl { name, .. } | Node::FunctionDecl { name, .. } => name,
                Node::Interface { name, .. } => Some(name),
                _ => None,
            };
            if let Some(n) = named
                && let Node::Ident { name } = *ast.node(n)
            {
                return ast.atom(name).to_owned();
            }
        }
        if name != "__class" {
            return name.to_owned();
        }
        // A class expression is named after what it initializes or is assigned to:
        // `const C = class {}`, `C = class {}` and `{ C: class {} }` (key as written).
        if let Some(d) = self.c.first_decl(sym)
            && let Some(p) = self.c.parent(d)
        {
            let named = match *self.c.node(p) {
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } if init == d.node => Some(name),
                Node::Assign {
                    left,
                    op: ts_r_ast::AssignOp::Assign,
                    right,
                } if right == d.node => Some(left),
                Node::Property { key, value, .. } if value == d.node => {
                    return self.c.text(Loc::new(d.file, key)).to_owned();
                }
                _ => None,
            };
            if let Some(name) = named
                && let Node::Ident { .. } = self.c.ast(d.file).node(name)
            {
                return self.c.text(Loc::new(d.file, name)).to_owned();
            }
        }
        "(Anonymous class)".to_owned()
    }

    /// Qualifies a type's name with its enclosing namespaces.
    fn qualified_name(&self, sym: SymId, name: String) -> String {
        if !self.print_qualified {
            return name;
        }
        if self.print_via_aliases
            && let Some(scope) = self.print_scope
            && let Some(q) = self.name_via_alias(sym, &name, scope)
        {
            return q;
        }
        let mut out = name;
        // The symbol named by the leftmost part of `out`.
        let mut head = sym;
        let mut cur = self.c.sym_parent(sym);
        let mut guard = 0;
        while let Some(p) = cur {
            guard += 1;
            if guard > 32 {
                break;
            }
            let f = self.c.sym_flags(p);
            // A module's export, told apart from a same-named one: `import("a").F`.
            if self.print_module_qualified
                && f.intersects(SymbolFlags::MODULE_FILE)
                && let Some(d) = self.c.first_decl(p)
            {
                let name = self.c.sym_name(p);
                let path = if !name.starts_with('"') {
                    strip_ts_ext(&self.c.file(d.file).path)
                } else {
                    name.trim_matches('"').to_owned()
                };
                out = format!("import(\"{path}\").{out}");
                break;
            }
            // `declare global { interface Response {} }` prints `Response`.
            if f.intersects(SymbolFlags::MODULE_FILE)
                || !f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
                || self.c.sym_name(p) == "__global"
            {
                break;
            }
            if let Some(scope) = self.print_scope {
                let span = self.c.span(scope);
                // Only a namespace or enum body has the exports in scope: inside a class
                // merged with a namespace, its members stay qualified (`typeof Path.Utils`).
                let inside = self.c.decls(p).iter().any(|d| {
                    d.file == scope.file
                        && matches!(
                            self.c.ast(d.file).node(d.node),
                            Node::Module { .. } | Node::Enum { .. }
                        )
                        && {
                            let ds = self.c.span(*d);
                            ds.start <= span.start && span.end <= ds.end
                        }
                });
                // ... unless its name means something else there (`typeof M.C` inside
                // `namespace C { export var C = M.C }`; tsc: getAccessibleSymbolChain).
                if inside && self.names_symbol_at(scope, head) {
                    break;
                }
            }
            out = format!("{}.{}", self.c.sym_name(p), out);
            head = p;
            cur = self.c.sym_parent(p);
        }
        out
    }

    /// Whether `sym`'s name, looked up at `scope`, finds `sym` (or a symbol with its
    /// declaration).
    fn names_symbol_at(&self, scope: Loc, sym: SymId) -> bool {
        let name = self.c.sym_name(sym);
        match self.resolve_name_at(scope, name, meaning::ALL) {
            Some(s) => {
                s == sym || {
                    let (a, b) = (self.c.first_decl(s), self.c.first_decl(sym));
                    a.is_some() && a == b
                }
            }
            None => true,
        }
    }

    /// `sym` named through an import alias visible at `scope` that resolves to it or to
    /// one of its namespaces, the nearest first (`a` for `import a = require("./a")` of
    /// `export = Foo`, `d.C` for `import d = M.N`, `ns.C` for `import * as ns`); `None`
    /// when `sym` or the namespace reached first is in scope by its own name, or no alias
    /// reaches it (tsc: checker.go getAccessibleSymbolChain, which tries each container
    /// from the symbol outwards, by name and then through aliases, in each scope's table).
    fn name_via_alias(&self, sym: SymId, name: &str, scope: Loc) -> Option<String> {
        let same = |a: SymId, b: SymId| {
            a == b || {
                let (da, db) = (self.c.first_decl(a), self.c.first_decl(b));
                da.is_some() && da == db
            }
        };
        let b = self.c.bound(scope.file);
        let ast = self.c.ast(scope.file);
        let parents = self.c.file(scope.file).parents();
        // The import aliases in scope, innermost scope first.
        let mut aliases: Vec<(String, SymId)> = Vec::new();
        let mut cur = Some(scope.node);
        let mut guard = 0;
        while let Some(node) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            if let Some(sc) = b.scope_of(node) {
                let sc = b.scopes[sc.0 as usize];
                for &(n, s) in b.table(sc.locals) {
                    let id = self.c.sym(scope.file, s);
                    if self.c.sym_flags(id).intersects(SymbolFlags::ALIAS)
                        && let Some(t) = self.c.resolve_alias(id)
                    {
                        aliases.push((b.name_text(ast, n).to_owned(), t));
                    }
                }
            }
            cur = parents.parent(node);
        }
        if aliases.is_empty() {
            return None;
        }
        let mut target = sym;
        let mut path = name.to_owned();
        for _ in 0..32 {
            let own = self.c.sym_name(target);
            let m = crate::symbols::symbol_meaning(self.c.sym_flags(target));
            let visible = self
                .c
                .lookup_in_scope_chain(scope, own, m)
                .or_else(|| self.c.global_symbol(own, m))
                .and_then(|v| self.c.resolve_alias(v))
                .is_some_and(|v| same(v, target));
            if visible {
                return None;
            }
            if let Some((alias, _)) = aliases.iter().find(|(_, t)| same(*t, target)) {
                return Some(if target == sym {
                    alias.clone()
                } else {
                    format!("{alias}.{path}")
                });
            }
            let parent = self.c.sym_parent(target)?;
            let f = self.c.sym_flags(parent);
            if !f.intersects(
                SymbolFlags::MODULE_FILE
                    | SymbolFlags::NAMESPACE_MODULE
                    | SymbolFlags::VALUE_MODULE,
            ) || self.c.sym_name(parent) == "__global"
            {
                return None;
            }
            if target != sym {
                path = format!("{}.{path}", self.c.sym_name(target));
            }
            target = parent;
        }
        None
    }

    /// The name of a symbol in `typeof C`: an unnamed class expression takes the name it is
    /// assigned to (`const C = class {}`, `{ C: class {} }`, `o.C = class {}`, and
    /// `exports` for `module.exports = class {}`, observed tsc 7).
    fn class_static_name(&self, sym: SymId) -> String {
        let name = self.c.sym_name(sym);
        if name != "__class" {
            return name.to_owned();
        }
        let inferred = self.c.first_decl(sym).and_then(|d| {
            let ast = self.c.ast(d.file);
            let p = self.c.parent(d)?;
            let ident = |n: ts_r_ast::NodeId| match *ast.node(n) {
                Node::Ident { name } => Some(ast.atom(name).to_owned()),
                _ => None,
            };
            match *ast.node(p.node) {
                Node::VarDeclarator { name, init, .. } if init == Some(d.node) => ident(name),
                Node::Property { key, value, .. } if value == d.node => ident(key),
                Node::Assign { left, right, .. } if right == d.node => match *ast.node(left) {
                    Node::Ident { name } => Some(ast.atom(name).to_owned()),
                    Node::Member { prop, .. } => ident(prop),
                    _ => None,
                },
                _ => None,
            }
        });
        // Otherwise as instances name it (a key as written, `(Anonymous class)`).
        inferred.unwrap_or_else(|| self.sym_display_name(sym))
    }

    /// `d` is the global `Iterable`, `AsyncIterable`, `IterableIterator` or
    /// `AsyncIterableIterator` interface.
    fn is_iterable_interface(&mut self, d: TypeId) -> bool {
        use crate::checker::GlobalType as G;
        [
            G::Iterable,
            G::AsyncIterable,
            G::IterableIterator,
            G::AsyncIterableIterator,
        ]
        .into_iter()
        .any(|g| self.global_type(g) == Some(d))
    }

    fn print_object(&mut self, t: TypeId, depth: u32, stack: &mut Vec<TypeId>) -> String {
        let s = self.store();
        let Some(o) = s.object_of(t) else {
            return "{}".into();
        };
        match o.kind {
            // The static side of a class without its signatures (heritage checks).
            ObjectKind::Literal { sym }
                if o.flags.contains(ts_r_types::ObjectFlags::CLASS_STATIC)
                    && sym != SymId::NONE =>
            {
                let n = self.class_static_name(sym);
                format!("typeof {}", self.qualified_name(sym, n))
            }
            ObjectKind::Declared {
                sym, type_params, ..
            } => {
                let name = self.sym_display_name(sym);
                let q = self.qualified_name(sym, name);
                let params = s.list_items(type_params).to_vec();
                let local = self.local_type_params(sym);
                if local.is_empty() {
                    return q;
                }
                let shown = &params[params.len() - local.len().min(params.len())..];
                // The generic `Array`'s own declared type prints as an array (`T[]`).
                if self.c.is_array_symbol(sym) && shown.len() == 1 {
                    let el = self.print_postfix_operand(shown[0], depth + 1, stack);
                    return if self.c.is_readonly_array_symbol(sym) {
                        format!("readonly {el}[]")
                    } else {
                        format!("{el}[]")
                    };
                }
                let parts: Vec<String> = shown
                    .iter()
                    .map(|&a| self.print_type(a, depth + 1, stack))
                    .collect();
                format!("{q}<{}>", parts.join(", "))
            }
            ObjectKind::Reference { target, args } => {
                let args = s.list_items(args).to_vec();
                if let Some(shape) = s.tuple_shape_of(target) {
                    let shape = shape.clone();
                    let mut parts = Vec::new();
                    for (i, el) in shape.elements.iter().enumerate() {
                        let mut a = args.get(i).copied().unwrap_or(TypeId::ANY);
                        // Optional elements read as `T | undefined` (`[string, (number |
                        // undefined)?]`, `[a?: number | undefined]`).
                        if el.kind == TupleElement::OPTIONAL
                            && !self.c.opts.exact_optional_property_types
                        {
                            a = self.add_optionality(a);
                        }
                        let label = el.label.map(|l| self.store().str(l).to_owned());
                        let ty = match el.kind {
                            TupleElement::REST => {
                                format!("{}[]", self.print_postfix_operand(a, depth + 1, stack))
                            }
                            // `(number | undefined)?`
                            TupleElement::OPTIONAL if el.label.is_none() => {
                                self.print_postfix_operand(a, depth + 1, stack)
                            }
                            _ => self.print_type(a, depth + 1, stack),
                        };
                        let item = match (el.kind, label) {
                            (TupleElement::OPTIONAL, Some(l)) => format!("{l}?: {ty}"),
                            (TupleElement::OPTIONAL, None) => format!("{ty}?"),
                            (TupleElement::REST | TupleElement::VARIADIC, Some(l)) => {
                                format!("...{l}: {ty}")
                            }
                            (TupleElement::REST | TupleElement::VARIADIC, None) => {
                                format!("...{ty}")
                            }
                            (_, Some(l)) => format!("{l}: {ty}"),
                            (_, None) => ty,
                        };
                        parts.push(item);
                    }
                    // Elements that would print alike (`[Foo.Yep, Bar.Yep]`) are qualified.
                    if !self.print_qualified
                        && parts
                            .iter()
                            .enumerate()
                            .any(|(i, p)| parts[..i].contains(p))
                    {
                        self.print_qualified = true;
                        let q = self.print_object(t, depth, stack);
                        self.print_qualified = false;
                        return q;
                    }
                    let body = format!("[{}]", parts.join(", "));
                    return if shape.readonly {
                        format!("readonly {body}")
                    } else {
                        body
                    };
                }
                let Some(ObjectKind::Declared {
                    sym, type_params, ..
                }) = s.object_of(target).map(|o| o.kind)
                else {
                    return "{}".into();
                };
                let n_all = s.list_items(type_params).len();
                let local_n = self.local_type_params(sym).len();
                let shown: Vec<TypeId> = args[..n_all.min(args.len())]
                    .iter()
                    .skip(n_all.saturating_sub(local_n))
                    .copied()
                    .collect();
                if self.c.is_array_symbol(sym) && shown.len() == 1 {
                    let el = self.print_postfix_operand(shown[0], depth + 1, stack);
                    return if self.c.is_readonly_array_symbol(sym) {
                        format!("readonly {el}[]")
                    } else {
                        format!("{el}[]")
                    };
                }
                let name = self.sym_display_name(sym);
                let q = self.qualified_name(sym, name);
                if shown.is_empty() {
                    return q;
                }
                // tsc omits trailing `any` arguments of the iterable interfaces
                // (`Iterable<number>` for `Iterable<number, any, any>`), not of `Iterator`,
                // `Generator` or other types with defaults.
                let mut shown = shown;
                if shown.len() > 1 && self.is_iterable_interface(target) {
                    while shown.len() > 1 && shown.last() == Some(&TypeId::ANY) {
                        shown.pop();
                    }
                }
                let parts: Vec<String> = shown
                    .iter()
                    .map(|&a| self.print_type(a, depth + 1, stack))
                    .collect();
                format!("{q}<{}>", parts.join(", "))
            }
            ObjectKind::Anonymous { decl, kind, .. } if decl.0 == u32::MAX => {
                let sym = SymId(decl.1);
                match kind {
                    anon::CLASS_STATIC | anon::NAMESPACE => self.typeof_symbol_name(sym),
                    anon::SYMBOL_FUNCTION => {
                        let f = self.c.sym_flags(sym);
                        if f.intersects(SymbolFlags::NAMESPACE) || self.has_expando(sym) {
                            let n = self.c.sym_name(sym).to_owned();
                            return format!("typeof {}", self.qualified_name(sym, n));
                        }
                        self.print_structure(t, depth, stack)
                    }
                    _ => self.print_structure(t, depth, stack),
                }
            }
            ObjectKind::Mapped { .. } if self.is_generic_mapped_type(t) => {
                self.print_mapped(t, depth, stack)
            }
            // A namespace import's view of a class, enum or namespace (`import * as ns`
            // of an `export =`): named by the symbol.
            ObjectKind::Literal { sym }
                if o.flags.contains(ts_r_types::ObjectFlags::MODULE) && sym != SymId::NONE =>
            {
                self.typeof_symbol_name(sym)
            }
            _ => self.print_structure(t, depth, stack),
        }
    }

    /// `typeof C` / `typeof NS` / `typeof import("m")` for a class, namespace or module.
    fn typeof_symbol_name(&mut self, sym: SymId) -> String {
        let f = self.c.sym_flags(sym);
        if f.intersects(SymbolFlags::MODULE_FILE) {
            let name = self.c.sym_name(sym).to_owned();
            let path = if let Some(d) = self.c.first_decl(sym) {
                let p = self.c.file(d.file).path.clone();
                if self.c.bound(d.file).file_symbol.0 == 0 && name == "__file" {
                    strip_ts_ext(&p)
                } else {
                    name.trim_matches('"').to_owned()
                }
            } else {
                name
            };
            return format!("typeof import(\"{path}\")");
        }
        let n = self.class_static_name(sym);
        format!("typeof {}", self.qualified_name(sym, n))
    }

    /// What a type met again while printed (or nested past the print depth) prints: `any`
    /// under noErrorTruncation (tsc's test harness), else `...`.
    fn elided(&self) -> &'static str {
        if self.c.opts.no_error_truncation {
            "any"
        } else {
            "..."
        }
    }

    /// `C extends E ? X : Y` with the conditional's mapper applied.
    fn print_conditional(&mut self, t: TypeId, depth: u32, stack: &mut Vec<TypeId>) -> String {
        let s = self.store();
        let Some(d) = s.conditional_data(t) else {
            return "any".into();
        };
        if stack.contains(&t) || depth > MAX_PRINT_DEPTH {
            return self.elided().into();
        }
        stack.push(t);
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
        let cs = self.print_wrapped(check, depth + 1, stack, false);
        let infer = s.list_items(root.infer_params).to_vec();
        let saved = self.print_infer.len();
        self.print_infer.extend(infer);
        let es = self.print_type(ext, depth + 1, stack);
        self.print_infer.truncate(saved);
        let ts = self.print_type(tt, depth + 1, stack);
        let fs = self.print_type(ft, depth + 1, stack);
        stack.pop();
        format!("{cs} extends {es} ? {ts} : {fs}")
    }

    /// `{ [K in C]: X; }` for a generic mapped type.
    fn print_mapped(&mut self, t: TypeId, depth: u32, stack: &mut Vec<TypeId>) -> String {
        let Some(info) = self.mapped_info(t) else {
            return "{}".into();
        };
        if stack.contains(&t) || depth > MAX_PRINT_DEPTH {
            return self.elided().into();
        }
        stack.push(t);
        let k = self.print_type(info.k, depth + 1, stack);
        let c = self.mapped_constraint(&info);
        // An uninstantiated intersection constraint is written as declared (`keyof T &
        // keyof Stuff`, not its distributed keys).
        let written = match info
            .constraint_node
            .map(|cn| *self.ast(info.decl.file).node(cn))
        {
            Some(Node::IntersectionType { types })
                if info.mapper == ts_r_types::MapperId::IDENTITY =>
            {
                // `keyof X` members as written, the others printed.
                let file = info.decl.file;
                let members = self.ast(file).list(types).to_vec();
                let mut parts = Vec::with_capacity(members.len());
                for m in members {
                    let text = match self.ast(file).node(m) {
                        Node::TypeOperator {
                            op: ts_r_ast::TypeOperatorKind::KeyOf,
                            ..
                        } => self.written_type_text(file, m, 0),
                        _ => None,
                    };
                    let text = match text {
                        Some(t) => t,
                        None => {
                            let mt = self.type_from_node(Loc::new(file, m));
                            self.print_wrapped(mt, depth + 1, stack, false)
                        }
                    };
                    parts.push(text);
                }
                Some(parts.join(" & "))
            }
            _ => None,
        };
        let cs = match written {
            Some(w) => w,
            None => self.print_type(c, depth + 1, stack),
        };
        let name = match info.name_node {
            Some(n) => {
                let raw = self.type_from_node(Loc::new(info.decl.file, n));
                let it = self.instantiate(raw, info.mapper);
                format!(" as {}", self.print_type(it, depth + 1, stack))
            }
            None => String::new(),
        };
        let tmpl = match info.template_node {
            Some(n) => {
                let raw = self.type_from_node(Loc::new(info.decl.file, n));
                let it = self.instantiate(raw, info.mapper);
                self.print_type(it, depth + 1, stack)
            }
            None => "any".into(),
        };
        stack.pop();
        use ts_r_ast::MappedModifier as M;
        let ro = match info.readonly {
            M::None => "",
            M::Present => "readonly ",
            M::Add => "+readonly ",
            M::Remove => "-readonly ",
        };
        let q = match info.optional {
            M::None => "",
            M::Present => "?",
            M::Add => "+?",
            M::Remove => "-?",
        };
        format!("{{ {ro}[{k} in {cs}{name}]{q}: {tmpl}; }}")
    }

    fn has_expando(&self, _sym: SymId) -> bool {
        false
    }

    /// `{ ... }` or an arrow form.
    fn print_structure(&mut self, t: TypeId, depth: u32, stack: &mut Vec<TypeId>) -> String {
        if stack.contains(&t) && self.c.opts.no_error_truncation {
            return "any".into();
        }
        if stack.contains(&t) || depth > MAX_PRINT_DEPTH {
            // A function's type within itself is `typeof f`.
            if let Some(ObjectKind::Anonymous { decl, kind, .. }) =
                self.store().object_of(t).map(|o| o.kind)
                && decl.0 == u32::MAX
                && kind == anon::SYMBOL_FUNCTION
                && stack.contains(&t)
            {
                let sym = SymId(decl.1);
                let n = self.c.sym_name(sym).to_owned();
                return format!("typeof {}", self.qualified_name(sym, n));
            }
            return self.elided().into();
        }
        stack.push(t);
        let m = self.resolved_members(t);
        let props: Vec<(ts_r_types::StrId, SymId)> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .collect();
        let call = m.call.to_vec();
        let construct = m.construct.to_vec();
        let index = m.index.to_vec();
        let out = if props.is_empty() && index.is_empty() && call.len() == 1 && construct.is_empty()
        {
            self.print_signature(call[0], SigKind::Call, true, depth, stack)
        } else if props.is_empty() && index.is_empty() && call.is_empty() && construct.len() == 1 {
            self.print_signature(construct[0], SigKind::Construct, true, depth, stack)
        } else {
            let mut items: Vec<String> = Vec::new();
            for &c in &call {
                items.push(format!(
                    "{};",
                    self.print_signature(c, SigKind::Call, false, depth, stack)
                ));
            }
            for &c in &construct {
                items.push(format!(
                    "new {};",
                    self.print_signature(c, SigKind::Call, false, depth, stack)
                ));
            }
            for i in &index {
                let k = self.print_type(i.key, depth + 1, stack);
                let v = self.print_type(i.value, depth + 1, stack);
                let ro = if i.readonly { "readonly " } else { "" };
                let name = i
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
                items.push(format!("{ro}[{name}: {k}]: {v};"));
            }
            for (n, p) in props {
                let name = self.store().str(n).to_owned();
                let pname = self
                    .declared_quoted_name(p)
                    .unwrap_or_else(|| self.property_name_for_print(&name));
                let pf = self.c.sym_flags(p);
                let optional = pf.intersects(SymbolFlags::OPTIONAL);
                let q = if optional { "?" } else { "" };
                if pf.intersects(SymbolFlags::METHOD) && !self.is_readonly_symbol(p) {
                    let pt = self.type_of_symbol(p);
                    let pt = if optional { self.non_nullable(pt) } else { pt };
                    let sigs = self.signatures_of(pt, SigKind::Call);
                    if !sigs.is_empty() && self.store().flags(pt).contains(TypeFlags::OBJECT) {
                        for sgn in sigs {
                            items.push(format!(
                                "{pname}{q}{};",
                                self.print_signature(sgn, SigKind::Call, false, depth, stack)
                            ));
                        }
                        continue;
                    }
                }
                let ro = if self.is_readonly_symbol(p) {
                    "readonly "
                } else {
                    ""
                };
                let mut pt = self.type_of_symbol(p);
                // tsc prints the `undefined` an optional property adds (`a?: string |
                // undefined`), except under exactOptionalPropertyTypes (the missing type).
                if optional
                    && self.c.opts.strict_null_checks
                    && self.c.opts.exact_optional_property_types
                {
                    pt = self.filter_type(pt, |_, x| x != TypeId::MISSING);
                    if self.store().flags(pt).contains(TypeFlags::NEVER) {
                        pt = TypeId::UNDEFINED;
                    }
                }
                // Editor printing leaves it out unless the annotation spells it out.
                if optional
                    && self.c.opts.strict_null_checks
                    && self.print_omit_optional_undefined
                    && !self.annotation_spells_undefined(p)
                {
                    let t2 = self.filter_type(pt, |ck, x| {
                        !ck.store().flags(x).contains(TypeFlags::UNDEFINED)
                    });
                    if !self.store().flags(t2).contains(TypeFlags::NEVER) {
                        pt = t2;
                    }
                }
                let reused = match self.member_annotation(p) {
                    Some(ann) if self.print_reuse => {
                        let declared = self.type_of_symbol(p);
                        self.reused_annotation(ann, declared, optional)
                    }
                    _ => None,
                };
                let ts = match (reused, self.error_annotation_text(p)) {
                    (Some(w), _) => w,
                    (None, Some(text)) if pt == TypeId::ERROR && !optional => text,
                    _ => self.print_type(pt, depth + 1, stack),
                };
                items.push(format!("{ro}{pname}{q}: {ts};"));
            }
            if items.is_empty() {
                "{}".to_owned()
            } else {
                format!("{{ {} }}", items.join(" "))
            }
        };
        stack.pop();
        out
    }

    /// The name of a type reference annotation that resolved to the error type (an
    /// unresolved import): tsc reuses the annotation when printing (`children: ReactNode`).
    fn type_ref_text(&self, l: Loc) -> Option<String> {
        let ast = self.ast(l.file);
        let Node::TypeRef {
            name,
            type_args: None,
        } = *ast.node(l.node)
        else {
            return None;
        };
        let t = crate::nodes::entity_text(ast, name);
        (!t.is_empty()).then_some(t)
    }

    /// A declared type parameter's constraint as written (tsc reuses the constraint node:
    /// `T extends Array<Base>`, `T extends Al`, union members in source order). `None` for
    /// instantiated copies and for node shapes outside references, keywords, arrays,
    /// unions, intersections and parentheses.
    fn written_constraint_text(&self, tp: TypeId) -> Option<String> {
        let data = self.store().type_param_data(tp)?;
        if data.origin.is_some() || data.is_this {
            return None;
        }
        let d = self.c.first_decl(data.sym)?;
        let Node::TypeParam {
            constraint: Some(c),
            ..
        } = *self.ast(d.file).node(d.node)
        else {
            return None;
        };
        self.written_type_text(d.file, c, 0)
    }

    /// An instantiated copy's constraint written `Array<X>` (or `ReadonlyArray<X>`) keeps
    /// that form with the instantiated element (`T extends Array<number>`): the name and
    /// the element type.
    fn written_array_constraint(&self, tp: TypeId, c: TypeId) -> Option<(String, TypeId)> {
        let s = self.store();
        let data = s.type_param_data(tp)?;
        data.origin?;
        let d = self.c.first_decl(data.sym)?;
        let ast = self.ast(d.file);
        let Node::TypeParam {
            constraint: Some(cn),
            ..
        } = *ast.node(d.node)
        else {
            return None;
        };
        let Node::TypeRef {
            name,
            type_args: Some(ta),
        } = *ast.node(cn)
        else {
            return None;
        };
        let text = crate::nodes::entity_text(ast, name);
        if text != "Array" && text != "ReadonlyArray" {
            return None;
        }
        let Node::TypeArgs { args } = *ast.node(ta) else {
            return None;
        };
        if ast.list(args).len() != 1 {
            return None;
        }
        let ObjectKind::Reference { target, args } = s.object_of(c)?.kind else {
            return None;
        };
        let Some(ObjectKind::Declared { sym, .. }) = s.object_of(target).map(|o| o.kind) else {
            return None;
        };
        if !self.c.is_array_symbol(sym) {
            return None;
        }
        let el = *s.list_items(args).first()?;
        Some((text, el))
    }

    fn written_type_text(
        &self,
        file: ts_r_diagnostics::FileId,
        n: ts_r_ast::NodeId,
        depth: u32,
    ) -> Option<String> {
        self.written_type_text_ext(file, n, depth, false)
    }

    /// [`Self::written_type_text`]; with `ext` (quick info's reused annotations) also
    /// function and constructor types, type literals, tuples, `typeof` queries, indexed
    /// access and `readonly`/`unique` operators, printed as tsc's emitter reprints them
    /// (`{ (x: number): number; }`).
    fn written_type_text_ext(
        &self,
        file: ts_r_diagnostics::FileId,
        n: ts_r_ast::NodeId,
        depth: u32,
        ext: bool,
    ) -> Option<String> {
        use ts_r_ast::KeywordTypeKind as K;
        if depth > 16 {
            return None;
        }
        let ast = self.ast(file);
        let join = |list: ts_r_ast::NodeList, sep: &str| -> Option<String> {
            let mut parts = Vec::new();
            for &m in ast.list(list) {
                parts.push(self.written_type_text_ext(file, m, depth + 1, ext)?);
            }
            Some(parts.join(sep))
        };
        let src = |x: ts_r_ast::NodeId| -> Option<String> {
            let sp = ast.span(x);
            self.c
                .file(file)
                .text
                .get(sp.start as usize..sp.end as usize)
                .map(str::to_owned)
        };
        if ext && let Some(w) = self.written_ext_shape(file, n, depth, &join, &src) {
            return w;
        }
        Some(match *ast.node(n) {
            Node::KeywordType { kind } => match kind {
                K::Any => "any",
                K::Unknown => "unknown",
                K::Number => "number",
                K::BigInt => "bigint",
                K::Boolean => "boolean",
                K::String => "string",
                K::Symbol => "symbol",
                K::Object => "object",
                K::Never => "never",
                K::Void => "void",
                K::Undefined => "undefined",
                K::Null => "null",
                K::Intrinsic | K::Const => return None,
            }
            .to_owned(),
            Node::TypeRef { name, type_args } => {
                let t = crate::nodes::entity_text(ast, name);
                if t.is_empty() {
                    return None;
                }
                // JSDoc's own names write the type they mean (`Array.<Object>` is
                // `Array<any>`), unless a local declaration of that name is meant.
                if type_args.is_none() && ast.is_jsdoc(name) {
                    let kw = match t.as_str() {
                        "String" => Some("string"),
                        "Number" => Some("number"),
                        "Boolean" => Some("boolean"),
                        "Void" => Some("void"),
                        "Undefined" => Some("undefined"),
                        "Null" => Some("null"),
                        "BigInt" => Some("bigint"),
                        "Object" if !self.c.opts.no_implicit_any => Some("any"),
                        _ => None,
                    };
                    if let Some(kw) = kw {
                        let l = Loc::new(file, name);
                        let local = self.c.resolve_ident(l, meaning::TYPE);
                        if local.is_none() || local == self.c.global_symbol(&t, meaning::TYPE) {
                            return Some(kw.to_owned());
                        }
                    }
                }
                match type_args {
                    None => t,
                    Some(ta) => {
                        let Node::TypeArgs { args } = *ast.node(ta) else {
                            return None;
                        };
                        format!("{t}<{}>", join(args, ", ")?)
                    }
                }
            }
            Node::ArrayType { elem } => {
                let e = self.written_type_text_ext(file, elem, depth + 1, ext)?;
                if matches!(
                    ast.node(elem),
                    Node::UnionType { .. } | Node::IntersectionType { .. }
                ) {
                    format!("({e})[]")
                } else {
                    format!("{e}[]")
                }
            }
            Node::UnionType { types } => join(types, " | ")?,
            Node::IntersectionType { types } => join(types, " & ")?,
            Node::ParenType { ty } => format!(
                "({})",
                self.written_type_text_ext(file, ty, depth + 1, ext)?
            ),
            // Literals keep their spelling (`'bye'`).
            Node::StringLit { .. } | Node::NumberLit { .. } => {
                let sp = ast.span(n);
                self.c
                    .file(file)
                    .text
                    .get(sp.start as usize..sp.end as usize)?
                    .to_owned()
            }
            Node::BoolLit { value } => if value { "true" } else { "false" }.to_owned(),
            Node::NullLit {} => "null".to_owned(),
            Node::TypeOperator {
                op: ts_r_ast::TypeOperatorKind::KeyOf,
                ty,
            } => format!(
                "keyof {}",
                self.written_type_text_ext(file, ty, depth + 1, ext)?
            ),
            _ => return None,
        })
    }

    /// The shapes [`Self::written_type_text_ext`] adds: `Some(None)` for one of them that
    /// cannot be written, `None` for other nodes.
    #[allow(clippy::type_complexity)]
    fn written_ext_shape(
        &self,
        file: ts_r_diagnostics::FileId,
        n: ts_r_ast::NodeId,
        depth: u32,
        join: &dyn Fn(ts_r_ast::NodeList, &str) -> Option<String>,
        src: &dyn Fn(ts_r_ast::NodeId) -> Option<String>,
    ) -> Option<Option<String>> {
        let ast = self.ast(file);
        let ty = |x: ts_r_ast::NodeId| self.written_type_text_ext(file, x, depth + 1, true);
        let sig = |tps: Option<ts_r_ast::NodeId>,
                   params: ts_r_ast::NodeList,
                   ret: Option<ts_r_ast::NodeId>,
                   arrow: bool|
         -> Option<String> {
            let mut out = String::new();
            if let Some(tps) = tps {
                let Node::TypeParams { params } = *ast.node(tps) else {
                    return None;
                };
                let mut parts = Vec::new();
                for &tp in ast.list(params) {
                    let Node::TypeParam {
                        modifiers,
                        name,
                        constraint,
                        default,
                    } = *ast.node(tp)
                    else {
                        return None;
                    };
                    if !modifiers.is_empty() {
                        return None;
                    }
                    let mut t = crate::nodes::ident_text(ast, name)?.to_owned();
                    if let Some(c) = constraint {
                        t.push_str(&format!(" extends {}", ty(c)?));
                    }
                    if let Some(d) = default {
                        t.push_str(&format!(" = {}", ty(d)?));
                    }
                    parts.push(t);
                }
                out.push_str(&format!("<{}>", parts.join(", ")));
            }
            let mut ps = Vec::new();
            for &p in ast.list(params) {
                let Node::Param {
                    rest,
                    pattern,
                    optional,
                    type_ann,
                    init: None,
                    ..
                } = *ast.node(p)
                else {
                    return None;
                };
                let name = crate::nodes::ident_text(ast, pattern)?;
                let t = match type_ann {
                    Some(a) => ty(a)?,
                    None => "any".to_owned(),
                };
                let dots = if rest { "..." } else { "" };
                let q = if optional { "?" } else { "" };
                ps.push(format!("{dots}{name}{q}: {t}"));
            }
            out.push_str(&format!("({})", ps.join(", ")));
            out.push_str(if arrow { " => " } else { ": " });
            out.push_str(&ty(ret?)?);
            Some(out)
        };
        let key = |k: ts_r_ast::NodeId| -> Option<String> {
            match *ast.node(k) {
                Node::Ident { .. } | Node::StringLit { .. } | Node::NumberLit { .. } => src(k),
                _ => None,
            }
        };
        Some(match *ast.node(n) {
            Node::FunctionType {
                type_params,
                params,
                return_type,
            } => sig(type_params, params, Some(return_type), true),
            Node::ConstructorType {
                is_abstract,
                type_params,
                params,
                return_type,
            } => sig(type_params, params, Some(return_type), true).map(|s| {
                let a = if is_abstract { "abstract " } else { "" };
                format!("{a}new {s}")
            }),
            Node::TypeLiteral { members } => {
                let mut items = Vec::new();
                for &m in ast.list(members) {
                    let item = match *ast.node(m) {
                        Node::PropertySignature {
                            modifiers,
                            key: k,
                            optional,
                            type_ann: Some(a),
                        } => {
                            let ro = if modifiers.contains(ts_r_ast::Modifiers::READONLY) {
                                "readonly "
                            } else {
                                ""
                            };
                            let q = if optional { "?" } else { "" };
                            Some(format!("{ro}{}{q}: {};", key(k)?, ty(a)?))
                        }
                        Node::MethodSignature {
                            kind: ts_r_ast::MethodKind::Method,
                            key: k,
                            optional,
                            type_params,
                            params,
                            return_type,
                            ..
                        } => {
                            let q = if optional { "?" } else { "" };
                            Some(format!(
                                "{}{q}{};",
                                key(k)?,
                                sig(type_params, params, return_type, false)?
                            ))
                        }
                        Node::CallSignature {
                            type_params,
                            params,
                            return_type,
                        } => Some(format!(
                            "{};",
                            sig(type_params, params, return_type, false)?
                        )),
                        Node::ConstructSignature {
                            type_params,
                            params,
                            return_type,
                        } => Some(format!(
                            "new {};",
                            sig(type_params, params, return_type, false)?
                        )),
                        _ => None,
                    };
                    let Some(item) = item else {
                        return Some(None);
                    };
                    items.push(item);
                }
                if items.is_empty() {
                    Some("{}".to_owned())
                } else {
                    Some(format!("{{ {} }}", items.join(" ")))
                }
            }
            Node::TupleType { elems } => join(elems, ", ").map(|e| format!("[{e}]")),
            Node::NamedTupleMember {
                rest,
                name,
                optional,
                ty: t,
            } => (|| {
                let nm = crate::nodes::ident_text(ast, name)?;
                let dots = if rest { "..." } else { "" };
                let q = if optional { "?" } else { "" };
                Some(format!("{dots}{nm}{q}: {}", ty(t)?))
            })(),
            Node::OptionalType { ty: t } => ty(t).map(|x| format!("{x}?")),
            Node::RestType { ty: t } => ty(t).map(|x| format!("...{x}")),
            Node::TypeQuery {
                expr_name,
                type_args: None,
            } => {
                let t = crate::nodes::entity_text(ast, expr_name);
                (!t.is_empty()).then(|| format!("typeof {t}"))
            }
            Node::IndexedAccessType { obj, index } => {
                (|| Some(format!("{}[{}]", ty(obj)?, ty(index)?)))()
            }
            Node::TypeOperator {
                op: op @ (ts_r_ast::TypeOperatorKind::Readonly | ts_r_ast::TypeOperatorKind::Unique),
                ty: t,
            } => {
                let o = if op == ts_r_ast::TypeOperatorKind::Readonly {
                    "readonly"
                } else {
                    "unique"
                };
                ty(t).map(|x| format!("{o} {x}"))
            }
            _ => return None,
        })
    }

    /// The type annotation of a parameter of a function or arrow expression (not a
    /// method's, whose body is a function expression under its `MethodDef`).
    /// The annotation of property `p`'s one declaration (a property signature or a class
    /// property), through the copies instantiation and spreads make.
    fn member_annotation(&self, p: SymId) -> Option<Loc> {
        let mut p = p;
        for _ in 0..8 {
            match self.c.synth_sym(p) {
                Some(x) if x.decl.is_none() && x.origins.len() == 1 => p = x.origins[0],
                _ => break,
            }
        }
        let [d] = self.c.decls(p)[..] else {
            return None;
        };
        match *self.ast(d.file).node(d.node) {
            Node::PropertySignature {
                type_ann: Some(t), ..
            }
            | Node::PropertyDef {
                type_ann: Some(t), ..
            } => Some(Loc::new(d.file, t)),
            _ => None,
        }
    }

    /// The annotation of parameter `p`'s declaration.
    fn param_annotation_of(&self, p: SymId) -> Option<Loc> {
        if p == SymId::NONE {
            return None;
        }
        let d = self.c.first_decl(p)?;
        match *self.ast(d.file).node(d.node) {
            Node::Param {
                type_ann: Some(t), ..
            } => Some(Loc::new(d.file, t)),
            _ => None,
        }
    }

    /// Whether the type predicate of `sig`'s declaration writes its type `t` in parentheses.
    fn predicate_annotation_parenthesized(&mut self, sig: SigId, t: TypeId) -> bool {
        let Some(d) = self.store().sig(sig).decl.and_then(Loc::unpack) else {
            return false;
        };
        let ast = self.ast(d.file);
        let r = match *ast.node(d.node) {
            Node::MethodSignature { return_type, .. } | Node::CallSignature { return_type, .. } => {
                return_type
            }
            Node::FunctionType { return_type, .. } => Some(return_type),
            _ => crate::nodes::fn_parts(ast, d.node).and_then(|f| f.return_type),
        };
        let Some(Node::TypePredicate { ty: Some(pt), .. }) = r.map(|r| *ast.node(r)) else {
            return false;
        };
        matches!(ast.node(pt), Node::ParenType { .. })
            && self.type_from_node(Loc::new(d.file, pt)) == t
    }

    /// The return annotation of signature `sig`'s declaration (not a type predicate).
    fn return_annotation_of(&self, sig: SigId) -> Option<Loc> {
        let d = self.store().sig(sig).decl.and_then(Loc::unpack)?;
        let ast = self.ast(d.file);
        let r = match *ast.node(d.node) {
            Node::MethodSignature { return_type, .. }
            | Node::CallSignature { return_type, .. }
            | Node::ConstructSignature { return_type, .. } => return_type,
            Node::FunctionType { return_type, .. } | Node::ConstructorType { return_type, .. } => {
                Some(return_type)
            }
            _ => crate::nodes::fn_parts(ast, d.node).and_then(|f| f.return_type),
        }?;
        if matches!(ast.node(r), Node::TypePredicate { .. }) {
            return None;
        }
        Some(Loc::new(d.file, r))
    }

    /// Annotation `ann` as written, when quick info reuses it for a member, parameter or
    /// return type of type `t` (see [`PrintMode::reuse_annotations`]): it writes `t` (for
    /// an `optional` declaration, `t` without `undefined`), names nothing that means
    /// something else at the print scope, has no `this` type, and is of a shape
    /// [`Self::written_type_text`] writes.
    fn reused_annotation(&mut self, ann: Loc, t: TypeId, optional: bool) -> Option<String> {
        let scope = self.print_scope?;
        if self.store().is_error(t) || t == TypeId::UNRESOLVED {
            return None;
        }
        let at = self.type_from_node(ann);
        let same = at == t
            || (optional && {
                let t2 = self.filter_type(t, |ck, x| {
                    !ck.store().flags(x).contains(TypeFlags::UNDEFINED)
                });
                t2 == at
            });
        if !same || !self.annotation_names_visible(ann, scope) {
            return None;
        }
        self.written_type_text_ext(ann.file, ann.node, 0, true)
    }

    /// Whether every name type node `ann` uses resolves at `scope` to what it resolves to
    /// where it is written (names declared inside it and type parameters excepted; a `this`
    /// type is never visible).
    fn annotation_names_visible(&self, ann: Loc, scope: Loc) -> bool {
        let ast = self.ast(ann.file);
        let span = ast.span(ann.node);
        let leftmost = |mut e: ts_r_ast::NodeId| {
            for _ in 0..1000 {
                match *ast.node(e) {
                    Node::QualifiedName { left, .. } => e = left,
                    Node::Member { obj, .. } => e = obj,
                    _ => break,
                }
            }
            e
        };
        let mut stack = vec![ann.node];
        let mut seen = 0u32;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > 2000 {
                return false;
            }
            let (id, m) = match *ast.node(n) {
                Node::ThisType {} | Node::This {} | Node::ImportType { .. } => return false,
                Node::TypeRef { name, .. } => {
                    let m = if matches!(ast.node(name), Node::QualifiedName { .. }) {
                        meaning::NAMESPACE
                    } else {
                        meaning::TYPE
                    };
                    (Some(leftmost(name)), m)
                }
                Node::TypeQuery { expr_name, .. } => (Some(leftmost(expr_name)), meaning::VALUE),
                _ => (None, 0),
            };
            if let Some(id) = id {
                let Some(name) = crate::nodes::ident_text(ast, id) else {
                    return false;
                };
                let Some(s) = self.resolve_name_at(Loc::new(ann.file, id), name, m) else {
                    return false;
                };
                let inside = self.c.first_decl(s).is_some_and(|d| {
                    d.file == ann.file && {
                        let ds = self.ast(d.file).span(d.node);
                        span.start <= ds.start && ds.end <= span.end
                    }
                });
                if !inside
                    && !self.c.sym_flags(s).intersects(SymbolFlags::TYPE_PARAMETER)
                    && self.resolve_name_at(scope, name, m) != Some(s)
                {
                    return false;
                }
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        true
    }

    fn expr_param_annotation(&self, p: SymId) -> Option<Loc> {
        if p == SymId::NONE {
            return None;
        }
        let l = self.c.first_decl(p)?;
        let Node::Param {
            type_ann: Some(ta), ..
        } = *self.ast(l.file).node(l.node)
        else {
            return None;
        };
        let f = self.c.parent(l)?;
        if !matches!(
            self.ast(f.file).node(f.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) || self
            .c
            .parent(f)
            .is_some_and(|m| matches!(self.ast(m.file).node(m.node), Node::MethodDef { .. }))
        {
            return None;
        }
        Some(Loc::new(l.file, ta))
    }

    /// The annotation `a` of a function expression's parameter names a type parameter of
    /// an enclosing declaration (out of scope where the type is printed: `(x: Array<T>)`
    /// inside `g<T>` prints `T[]`).
    fn names_outer_type_param(&self, a: Loc) -> bool {
        let ast = self.ast(a.file);
        let mut names = Vec::new();
        collect_type_ref_names(ast, a.node, &mut names, 0);
        if names.is_empty() {
            return false;
        }
        // Skip the parameter and its own function (whose type parameters print with it).
        let mut cur = self.c.parent(a).and_then(|p| self.c.parent(p));
        let mut steps = 0;
        while let Some(l) = cur.and_then(|f| self.c.parent(f)) {
            steps += 1;
            if steps > 256 {
                return true;
            }
            let tps = match *ast.node(l.node) {
                Node::ClassDecl { type_params, .. }
                | Node::ClassExpr { type_params, .. }
                | Node::Interface { type_params, .. }
                | Node::TypeAlias { type_params, .. } => type_params,
                _ => crate::nodes::fn_parts(ast, l.node).and_then(|f| f.type_params),
            };
            if let Some(tps) = tps
                && let Node::TypeParams { params } = *ast.node(tps)
            {
                for &tp in ast.list(params) {
                    if let Node::TypeParam { name, .. } = *ast.node(tp)
                        && names.contains(&crate::nodes::entity_text(ast, name))
                    {
                        return true;
                    }
                }
            }
            cur = Some(l);
        }
        false
    }

    fn error_annotation_text(&self, p: SymId) -> Option<String> {
        if p == SymId::NONE {
            return None;
        }
        let d = self
            .c
            .synth_sym(p)
            .and_then(|x| x.decl)
            .or_else(|| self.c.first_decl(p))?;
        let ast = self.ast(d.file);
        let ann = match *ast.node(d.node) {
            Node::PropertySignature { type_ann, .. }
            | Node::PropertyDef { type_ann, .. }
            | Node::Param { type_ann, .. } => type_ann?,
            _ => return None,
        };
        self.type_ref_text(Loc::new(d.file, ann))
    }

    fn error_return_annotation_text(&self, sig: SigId) -> Option<String> {
        let (f, n) = self.store().sig(sig).decl?;
        let file = ts_r_diagnostics::FileId(f);
        let node = ts_r_ast::NodeId::from_raw(n)?;
        let ast = self.ast(file);
        let rt = match *ast.node(node) {
            Node::FunctionType { return_type, .. } => Some(return_type),
            _ => crate::nodes::fn_parts(ast, node).and_then(|p| p.return_type),
        }?;
        self.type_ref_text(Loc::new(file, rt))
    }

    /// A property declared with a string literal name prints with the literal's own
    /// quotes (`'aria-label': string`).
    pub(crate) fn declared_quoted_name(&self, p: SymId) -> Option<String> {
        let d = self
            .c
            .synth_sym(p)
            .and_then(|x| x.decl)
            .or_else(|| self.c.first_decl(p))?;
        let ast = self.ast(d.file);
        let key = match *ast.node(d.node) {
            Node::PropertySignature { key, .. }
            | Node::PropertyDef { key, .. }
            | Node::MethodSignature { key, .. }
            | Node::Property { key, .. } => key,
            _ => return None,
        };
        // A computed name of a string literal type is quoted even when numeric
        // (`{ [c0]: 1 }` with `const c0 = "1"` prints `{ "1": number; }`).
        if let Node::ComputedKey { expr } = *ast.node(key) {
            if matches!(ast.node(expr), Node::StringLit { .. }) {
                let name = crate::nodes::key_text(ast, expr)?;
                return (!is_identifier_name(&name)).then(|| quote_string(&name));
            }
            let t = *self.node_types.get(&Loc::new(d.file, expr))?;
            if !self.store().flags(t).contains(TypeFlags::STRING_LITERAL) {
                return None;
            }
            let name = self.store().string_value(t)?;
            return (!is_identifier_name(name)).then(|| quote_string(name));
        }
        if !matches!(ast.node(key), Node::StringLit { .. }) {
            return None;
        }
        let name = crate::nodes::key_text(ast, key)?;
        if is_identifier_name(&name) {
            return None;
        }
        let sp = self.c.span(Loc::new(d.file, key));
        let text = self
            .c
            .file(d.file)
            .text
            .get(sp.start as usize..sp.end as usize)?;
        Some(text.to_owned())
    }

    pub(crate) fn property_name_for_print(&self, name: &str) -> String {
        if let Some(d) = self.c.symbol_key_display(name) {
            return d;
        }
        if is_identifier_name(name) || name.starts_with('#') {
            return name.to_owned();
        }
        if !name.is_empty()
            && name
                .parse::<f64>()
                .is_ok_and(|v| ts_r_binder::js_number_to_string(v) == name)
        {
            return name.to_owned();
        }
        quote_string(name)
    }

    /// `(a: T) => R` (arrow) or `(a: T): R` (member form).
    /// The parameters a rest parameter of a mutable, non-variadic tuple type stands for.
    fn expanded_rest_params(
        &mut self,
        name: &str,
        ty: TypeId,
        depth: u32,
        stack: &mut Vec<TypeId>,
    ) -> Option<Vec<String>> {
        let s = self.store();
        let ObjectKind::Reference { target, .. } = s.object_of(ty)?.kind else {
            return None;
        };
        let shape = s.tuple_shape_of(target)?.clone();
        if shape.readonly
            || shape
                .elements
                .iter()
                .any(|e| e.kind == TupleElement::VARIADIC)
        {
            return None;
        }
        let args = self.c.type_args_of_tuple(ty).to_vec();
        let mut out = Vec::with_capacity(shape.elements.len());
        for (i, el) in shape.elements.iter().enumerate() {
            let a = args.get(i).copied().unwrap_or(TypeId::ANY);
            // Unlabeled: `x_0`, `x_1`, and the rest element keeps the parameter's name.
            let label = match el.label {
                Some(l) => self.store().str(l).to_owned(),
                None if el.kind == TupleElement::REST => name.to_owned(),
                None => format!("{name}_{i}"),
            };
            out.push(match el.kind {
                TupleElement::OPTIONAL => {
                    let a = self.add_optionality(a);
                    format!("{label}?: {}", self.print_type(a, depth + 1, stack))
                }
                TupleElement::REST => {
                    let ts = self.print_postfix_operand(a, depth + 1, stack);
                    format!("...{label}: {ts}[]")
                }
                _ => format!("{label}: {}", self.print_type(a, depth + 1, stack)),
            });
        }
        Some(out)
    }

    /// Whether `sym`'s declaration annotates it with `undefined` or a union that lists
    /// `undefined` (`x?: string | undefined`).
    fn annotation_spells_undefined(&self, sym: SymId) -> bool {
        if sym == SymId::NONE {
            return false;
        }
        let Some(d) = self.c.first_decl(sym) else {
            return false;
        };
        let ast = self.ast(d.file);
        let ann = match *ast.node(d.node) {
            Node::Param { type_ann, .. }
            | Node::PropertySignature { type_ann, .. }
            | Node::PropertyDef { type_ann, .. } => type_ann,
            _ => None,
        };
        let Some(mut n) = ann else {
            return false;
        };
        for _ in 0..64 {
            match *ast.node(n) {
                Node::ParenType { ty } => n = ty,
                _ => break,
            }
        }
        let is_undef = |n: ts_r_ast::NodeId| {
            matches!(
                ast.node(n),
                Node::KeywordType {
                    kind: ts_r_ast::KeywordTypeKind::Undefined
                }
            )
        };
        match *ast.node(n) {
            Node::UnionType { types } => ast.list(types).iter().any(|&m| is_undef(m)),
            _ => is_undef(n),
        }
    }

    pub fn print_signature(
        &mut self,
        sig: SigId,
        kind: SigKind,
        arrow: bool,
        depth: u32,
        stack: &mut Vec<TypeId>,
    ) -> String {
        let s = self.store();
        let d = s.sig(sig);
        let mut out = String::new();
        if arrow && d.flags.contains(SigFlags::CONSTRUCT) || kind == SigKind::Construct {
            if d.flags.contains(SigFlags::ABSTRACT) {
                out.push_str("abstract ");
            }
            out.push_str("new ");
        }
        let tps = s.list_items(d.type_params).to_vec();
        if !tps.is_empty() {
            let mut parts = Vec::new();
            for tp in tps {
                let name = self.print_type(tp, depth + 1, stack);
                let c = self.type_param_constraint(tp);
                match c {
                    Some(c) => {
                        let cs = match self.written_constraint_text(tp) {
                            Some(w) => w,
                            None => match self.written_array_constraint(tp, c) {
                                Some((name, el)) => {
                                    format!("{name}<{}>", self.print_type(el, depth + 1, stack))
                                }
                                None => self.print_type(c, depth + 1, stack),
                            },
                        };
                        parts.push(format!("{name} extends {cs}"));
                    }
                    None => parts.push(name),
                }
            }
            out.push('<');
            out.push_str(&parts.join(", "));
            out.push('>');
        }
        let d = self.store().sig(sig);
        let params = d.params.to_vec();
        let this_type = d.this_type;
        let has_rest = d.has_rest();
        let min = d.min_args as usize;
        let mut ps = Vec::new();
        if let Some(tt) = this_type {
            ps.push(format!("this: {}", self.print_type(tt, depth + 1, stack)));
        }
        let n = params.len();
        for (i, p) in params.iter().enumerate() {
            let name = self.store().str(p.name).to_owned();
            let rest = has_rest && i == n - 1;
            let mut ty = p.ty;
            let optional = !rest && i >= min && p.optional;
            // tsc keeps the `undefined` a `?` parameter adds (`radix?: number | undefined`)
            // and drops the one an initializer adds (`x?: number` for `x = 1`).
            let question = p.sym != SymId::NONE
                && self.c.first_decl(p.sym).is_some_and(|l| {
                    matches!(
                        *self.ast(l.file).node(l.node),
                        Node::Param { optional: true, .. }
                    )
                });
            // A function or arrow expression's parameter prints as annotated (`x?: string`,
            // `x: 'bye'`) while its signature is not instantiated.
            let ann = if self.store().sig(sig).target.is_none() {
                self.expr_param_annotation(p.sym)
            } else {
                None
            };
            let written = match ann {
                Some(a) if !self.names_outer_type_param(a) => {
                    self.written_type_text(a.file, a.node, 0).map(|w| {
                        // A JSDoc `{T=}` is written `T | undefined` (observed tsc 7, with
                        // or without `strictNullChecks`).
                        if !self.is_jsdoc_optional_annotation(a) {
                            return w;
                        }
                        if matches!(
                            self.ast(a.file).node(a.node),
                            Node::FunctionType { .. } | Node::ConstructorType { .. }
                        ) {
                            format!("({w}) | undefined")
                        } else {
                            format!("{w} | undefined")
                        }
                    })
                }
                _ => None,
            };
            let as_annotated = optional
                && question
                && self.c.opts.strict_null_checks
                && ann.is_some_and(|a| {
                    // An annotation that spells out `undefined` keeps it.
                    let ast = self.ast(a.file);
                    let is_undef = |n: ts_r_ast::NodeId| {
                        matches!(
                            ast.node(n),
                            Node::KeywordType {
                                kind: ts_r_ast::KeywordTypeKind::Undefined
                            }
                        )
                    };
                    match ast.node(a.node) {
                        Node::UnionType { types } => !ast.list(*types).iter().any(|&n| is_undef(n)),
                        _ => !is_undef(a.node),
                    }
                });
            // Editor printing drops the `undefined` a `?` adds unless the annotation
            // spells it out.
            let omit = self.print_omit_optional_undefined
                && question
                && !self.annotation_spells_undefined(p.sym);
            if optional && self.c.opts.strict_null_checks && (!question || as_annotated || omit) {
                let t2 = self.filter_type(ty, |ck, x| {
                    !ck.store().flags(x).contains(TypeFlags::UNDEFINED)
                });
                if !self.store().flags(t2).contains(TypeFlags::NEVER) {
                    ty = t2;
                }
            }
            // A rest parameter of a fixed tuple type prints as the parameters it stands for
            // (`(x: string, y?: boolean | undefined)`, unlabeled elements as `args_0`).
            if rest && let Some(expanded) = self.expanded_rest_params(&name, ty, depth, stack) {
                ps.extend(expanded);
                continue;
            }
            let reused = match self.param_annotation_of(p.sym) {
                Some(a) if self.print_reuse && written.is_none() => {
                    self.reused_annotation(a, p.ty, optional && question)
                }
                _ => None,
            };
            let written = written.or(reused);
            let ts = match self.error_annotation_text(p.sym) {
                Some(text)
                    if ty == TypeId::ERROR && p.sym != SymId::NONE && !optional && !question =>
                {
                    text
                }
                _ => match written {
                    Some(w) if ty != TypeId::ERROR => w,
                    _ => self.print_type(ty, depth + 1, stack),
                },
            };
            if rest {
                ps.push(format!("...{name}: {ts}"));
            } else if optional {
                ps.push(format!("{name}?: {ts}"));
            } else {
                ps.push(format!("{name}: {ts}"));
            }
        }
        out.push('(');
        out.push_str(&ps.join(", "));
        out.push(')');
        out.push_str(if arrow { " => " } else { ": " });
        let pred = self.predicate_of(sig);
        let r = self.return_type_of(sig);
        let rs = match pred {
            Some(p) => {
                let pname = match p.kind {
                    ts_r_types::PredicateKind::This | ts_r_types::PredicateKind::AssertsThis => {
                        "this".to_owned()
                    }
                    _ => self.store().str(p.param_name).to_owned(),
                };
                let asserts = matches!(
                    p.kind,
                    ts_r_types::PredicateKind::AssertsIdentifier
                        | ts_r_types::PredicateKind::AssertsThis
                );
                let ts = p.ty.map(|t| {
                    let ts = self.print_type(t, depth + 1, stack);
                    // Quick info keeps the declaration's parentheses (`this is (A & this)`).
                    if self.print_reuse && self.predicate_annotation_parenthesized(sig, t) {
                        format!("({ts})")
                    } else {
                        ts
                    }
                });
                match (asserts, ts) {
                    (true, None) => format!("asserts {pname}"),
                    (true, Some(ts)) => format!("asserts {pname} is {ts}"),
                    (false, Some(ts)) => format!("{pname} is {ts}"),
                    (false, None) => self.print_type(r, depth + 1, stack),
                }
            }
            None => match self.error_return_annotation_text(sig) {
                Some(text) if r == TypeId::ERROR => text,
                _ => match self.return_annotation_of(sig) {
                    Some(a) if self.print_reuse => match self.reused_annotation(a, r, false) {
                        Some(w) => w,
                        None => self.print_type(r, depth + 1, stack),
                    },
                    _ => self.print_type(r, depth + 1, stack),
                },
            },
        };
        out.push_str(&rs);
        out
    }

    /// The printed form of a signature in arrow form (for diagnostics).
    pub fn signature_to_string(&mut self, sig: SigId) -> String {
        let mut stack = Vec::new();
        let kind = if self.store().sig(sig).flags.contains(SigFlags::CONSTRUCT) {
            SigKind::Construct
        } else {
            SigKind::Call
        };
        let s = self.print_signature(sig, kind, false, 0, &mut stack);
        truncate(s)
    }

    pub fn object_flags_of(&self, t: TypeId) -> ObjectFlags {
        self.store()
            .object_of(t)
            .map_or(ObjectFlags::NONE, |o| o.flags)
    }
}

pub(crate) fn strip_ts_ext(p: &str) -> String {
    for ext in [
        ".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs",
    ] {
        if let Some(x) = p.strip_suffix(ext) {
            return x.to_owned();
        }
    }
    p.to_owned()
}

/// tsc truncates long type strings in diagnostics.
fn truncate(s: String) -> String {
    s
}

/// The simple names of the type references in a type node.
fn collect_type_ref_names(
    ast: &ts_r_ast::Ast,
    n: ts_r_ast::NodeId,
    out: &mut Vec<String>,
    depth: u32,
) {
    if depth > 32 {
        return;
    }
    match *ast.node(n) {
        Node::TypeRef { name, type_args } => {
            out.push(crate::nodes::entity_text(ast, name));
            if let Some(ta) = type_args
                && let Node::TypeArgs { args } = *ast.node(ta)
            {
                for &m in ast.list(args) {
                    collect_type_ref_names(ast, m, out, depth + 1);
                }
            }
        }
        Node::ArrayType { elem } => collect_type_ref_names(ast, elem, out, depth + 1),
        Node::ParenType { ty } => collect_type_ref_names(ast, ty, out, depth + 1),
        Node::UnionType { types } | Node::IntersectionType { types } => {
            for &m in ast.list(types) {
                collect_type_ref_names(ast, m, out, depth + 1);
            }
        }
        _ => {}
    }
}
