//! The quick-info text hover shows: one line per meaning of the symbol under the cursor.
//!
//! Rules learned from tsc 7's quick info (`tsc: tsc/internal/ls/hover.go`), written here on
//! our checker:
//!
//! - The location picks the meanings: a value reference shows the value meanings (and call
//!   signatures), a type reference the type meanings, the left of a qualified type name the
//!   namespace meanings; a declaration name shows its declaration's meaning (classes and
//!   enums, both value and type, show everything). When nothing matches, every meaning shows.
//! - An alias shows its target's meanings first, each line prefixed `(alias) `.
//! - Each meaning is a line, in this order: variable / property / accessor / parameter;
//!   enum member; function / method signatures; class / interface (a `new X()` or `X()`
//!   callee shows the resolved constructor `constructor X<string>(...)`); enum; namespace or
//!   module; type parameter (with the declaration it belongs to: `in C<T>`,
//!   `in f<T>(x: T): void`, `in type A<T>`); type alias.
//! - A variable or property that is the callee of a call or `new` shows the signature the
//!   call resolved to, arrow style.
//! - Functions and methods: on a declaration's name, that declaration's signature; at a
//!   call of an overloaded or generic function, the resolved signature with its type
//!   arguments (`f<string>(x: string)`); elsewhere every signature on its own line, the
//!   fourth onwards replaced by `// +N more overloads` when there are five or more.
//! - Names carry their owner (`C.m`, `G<T>.p`) and the namespaces around them (`N.f`) that
//!   do not enclose the location. An alias's target whose name does not reach the location
//!   (it lives in another module) shows under the alias's own name.

use ts_r_ast::{ModuleKind, Node, VarKind};
use ts_r_binder::{SymbolFacts, SymbolFlags, decl_name_node, meaning};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_types::{MapperId, SigId, SymId, TypeId, TypeKind};

/// The quick info of `s` named at `at`; `is_decl` when `at` declares `s`.
pub fn quick_info(ck: &mut Ck<'_, '_>, s: SymId, at: Loc, is_decl: bool) -> String {
    let m = location_meaning(ck, at, is_decl);
    let mut w = Writer {
        out: String::new(),
        alias: None,
        visited: Vec::new(),
        at,
        meaning: m,
        is_decl,
    };
    w.symbol(ck, s, 0);
    w.out
}

struct Writer {
    out: String,
    /// The alias being described through, while writing its target's lines.
    alias: Option<SymId>,
    visited: Vec<SymId>,
    at: Loc,
    meaning: u8,
    /// `at` declares the symbol: its name is not qualified by namespaces.
    is_decl: bool,
}

/// What meanings the name at `at` refers to (`meaning::VALUE`, `TYPE`, `NAMESPACE`, `ALL`).
fn location_meaning(ck: &Ck<'_, '_>, at: Loc, is_decl: bool) -> u8 {
    let c = ck.c;
    let Some(p) = c.parent(at) else {
        return meaning::ALL;
    };
    let n = at.node;
    if is_decl {
        return match *c.node(p) {
            Node::Interface { .. } | Node::TypeAlias { .. } | Node::TypeParam { .. } => {
                meaning::TYPE
            }
            Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Enum { .. } => meaning::ALL,
            Node::Module { .. } => {
                let inst = c
                    .sym_of_decl(p)
                    .is_some_and(|s| c.sym_flags(s).intersects(SymbolFlags::VALUE_MODULE));
                if inst {
                    meaning::ALL
                } else {
                    meaning::NAMESPACE
                }
            }
            Node::ImportSpecifier { .. }
            | Node::ImportDefaultSpecifier { .. }
            | Node::ImportNamespaceSpecifier { .. }
            | Node::ImportEquals { .. }
            | Node::ExportSpecifier { .. } => meaning::ALL,
            _ => meaning::VALUE,
        };
    }
    match *c.node(p) {
        Node::TypeRef { name, .. } if name == n => meaning::TYPE,
        Node::QualifiedName { left, right } => {
            if left == n {
                meaning::NAMESPACE
            } else if right == n {
                // `A.B` in a type reference names a type; elsewhere (`typeof A.B`,
                // `import x = A.B`) everything.
                match c.parent(p).map(|g| c.node(g)) {
                    Some(Node::TypeRef { .. }) => meaning::TYPE,
                    _ => meaning::ALL,
                }
            } else {
                meaning::ALL
            }
        }
        Node::ExprWithTypeArgs { expr, .. } if expr == n => {
            let class = c.parent(p).is_some_and(|h| {
                matches!(
                    c.node(h),
                    Node::HeritageClause {
                        kind: ts_r_ast::HeritageKind::Extends,
                        ..
                    }
                ) && c.parent(h).is_some_and(|k| {
                    matches!(c.node(k), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                })
            });
            if class { meaning::VALUE } else { meaning::TYPE }
        }
        Node::ImportSpecifier { .. }
        | Node::ExportSpecifier { .. }
        | Node::ImportEquals { .. }
        | Node::ExportAssignment { .. } => meaning::ALL,
        _ => meaning::VALUE,
    }
}

/// The symbol flags a meaning shows.
fn flags_for(m: u8) -> SymbolFlags {
    let mut f = SymbolFlags::NONE;
    if m & meaning::VALUE != 0 {
        f |= SymbolFlags::VALUE | SymbolFlags::SIGNATURE;
    }
    if m & meaning::TYPE != 0 {
        f |= SymbolFlags::TYPE;
    }
    if m & meaning::NAMESPACE != 0 {
        f |= SymbolFlags::NAMESPACE;
    }
    f
}

/// The call or `new` whose callee is the name at `at` (or `x.name`).
pub fn call_of(ck: &Ck<'_, '_>, at: Loc) -> Option<Loc> {
    let c = ck.c;
    let mut n = at;
    let p = c.parent(n)?;
    if let Node::Member { prop, .. } = *c.node(p)
        && prop == n.node
    {
        n = p;
    }
    let p = c.parent(n)?;
    match *c.node(p) {
        Node::Call { callee, .. } | Node::New { callee, .. } if callee == n.node => Some(p),
        _ => None,
    }
}

impl Writer {
    fn line(&mut self) {
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        if self.alias.is_some() {
            self.out.push_str("(alias) ");
        }
    }

    fn symbol(&mut self, ck: &mut Ck<'_, '_>, s: SymId, depth: u32) {
        let c = ck.c;
        let all = c.sym_flags(s);
        if all.intersects(SymbolFlags::ALIAS) && !self.visited.contains(&s) && depth < 8 {
            self.visited.push(s);
            if let Some(t) = c.resolve_alias(s)
                && t != s
            {
                let saved = (self.alias, self.is_decl);
                if self.alias.is_none() {
                    self.alias = Some(s);
                }
                self.is_decl = false;
                if c.sym_flags(t).intersects(SymbolFlags::MODULE_FILE) {
                    self.module_alias(ck, s);
                } else {
                    self.symbol(ck, t, depth + 1);
                }
                (self.alias, self.is_decl) = saved;
            }
        }
        let shown = SymbolFlags::VALUE | SymbolFlags::SIGNATURE | SymbolFlags::TYPE;
        let shown = shown | SymbolFlags::NAMESPACE | SymbolFlags::CONSTRUCTOR;
        let mut flags = all & (flags_for(self.meaning) | SymbolFlags::CONSTRUCTOR);
        if flags.is_empty() {
            if self.alias.is_some() || !self.out.is_empty() {
                return;
            }
            flags = all & shown;
        }
        if all.intersects(SymbolFlags::MODULE_FILE) && self.alias.is_none() {
            self.line();
            let path = c
                .first_decl(s)
                .map(|d| c.file(d.file).path.clone())
                .unwrap_or_default();
            let name = c.sym_name(s);
            if name.starts_with('"') {
                self.out.push_str(&format!("module {name}"));
            } else {
                self.out
                    .push_str(&format!("module \"{}\"", strip_ts_ext(&path)));
            }
            return;
        }
        if flags.intersects(SymbolFlags::VARIABLE | SymbolFlags::PROPERTY | SymbolFlags::ACCESSOR) {
            self.variable(ck, s, flags);
        }
        if flags.intersects(SymbolFlags::ENUM_MEMBER) {
            self.enum_member(ck, s);
        }
        if flags.intersects(SymbolFlags::FUNCTION | SymbolFlags::METHOD) {
            self.functions(ck, s, flags.intersects(SymbolFlags::METHOD));
        }
        if flags.intersects(SymbolFlags::CONSTRUCTOR) && !flags.intersects(SymbolFlags::CLASS) {
            self.constructor_member(ck, s);
        }
        if flags.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE) {
            self.class_or_interface(ck, s, flags);
        }
        if flags.intersects(SymbolFlags::ENUM) {
            self.line();
            if all.intersects(SymbolFlags::CONST_ENUM) {
                self.out.push_str("const ");
            }
            let name = self.name(ck, s);
            self.out.push_str(&format!("enum {name}"));
        }
        if flags.intersects(SymbolFlags::MODULE) {
            self.line();
            let ambient = c.decls(s).iter().any(|d| {
                matches!(*c.node(*d), Node::Module { name, kind: ModuleKind::Module, .. }
                    if matches!(c.ast(d.file).node(name), Node::StringLit { .. }))
            });
            let kw = if ambient { "module" } else { "namespace" };
            let name = self.name(ck, s);
            self.out.push_str(&format!("{kw} {name}"));
        }
        if flags.intersects(SymbolFlags::TYPE_PARAMETER) {
            self.type_parameter(ck, s);
        }
        if flags.intersects(SymbolFlags::TYPE_ALIAS) {
            self.line();
            let name = self.name(ck, s);
            let tps = self.type_params(ck, s);
            let body = alias_body(ck, s);
            self.out.push_str(&format!("type {name}{tps} = {body}"));
        }
        if flags.intersects(SymbolFlags::SIGNATURE) {
            self.line();
            let t = ck.type_of_symbol(s);
            let ts = ck.type_to_string_at(t, self.at);
            self.out.push_str(&ts);
        }
    }

    /// `import m = require("m")` / `import * as m`: `(alias) module m` / `namespace m`.
    fn module_alias(&mut self, ck: &Ck<'_, '_>, s: SymId) {
        let c = ck.c;
        let equals = c.first_decl(s).is_some_and(|d| {
            let parents = c.file(d.file).parents();
            [Some(d.node), parents.parent(d.node)]
                .into_iter()
                .flatten()
                .any(|n| matches!(c.ast(d.file).node(n), Node::ImportEquals { .. }))
        });
        self.line();
        let kw = if equals { "module" } else { "namespace" };
        self.out.push_str(&format!("{kw} {}", c.sym_name(s)));
    }

    fn variable(&mut self, ck: &mut Ck<'_, '_>, s: SymId, flags: SymbolFlags) {
        let c = ck.c;
        self.line();
        let auto_accessor = c.decls(s).iter().any(|d| {
            matches!(*c.node(*d), Node::PropertyDef { modifiers, .. }
                if modifiers.contains(ts_r_ast::Modifiers::ACCESSOR))
        });
        let prefix = if flags.intersects(SymbolFlags::PROPERTY) && !auto_accessor {
            "(property) "
        } else if flags.intersects(SymbolFlags::ACCESSOR) || auto_accessor {
            "(accessor) "
        } else {
            var_prefix(ck, s)
        };
        let name = self.name(ck, s);
        let opt = if c.sym_flags(s).intersects(SymbolFlags::OPTIONAL) {
            "?"
        } else {
            ""
        };
        self.out.push_str(&format!("{prefix}{name}{opt}: "));
        // The callee of a call or `new`: the signature the call resolved to.
        if let Some(call) = call_of(ck, self.at)
            && let Some(sig) = ck.resolved_signature(call)
        {
            let kind = if matches!(c.node(call), Node::New { .. }) {
                SigKind::Construct
            } else {
                SigKind::Call
            };
            let args = sig_type_args(ck, sig);
            let mut stack = Vec::new();
            let text = ck.print_signature(sig, kind, true, 0, &mut stack);
            let text = match (args.is_empty(), text.strip_prefix("new ")) {
                (false, Some(rest)) => format!("new {args}{rest}"),
                (false, None) => format!("{args}{text}"),
                _ => text,
            };
            self.out.push_str(&drop_optional_undefined(&text));
            return;
        }
        // Through an alias, tsc shows the target's declared type (no narrowing).
        let t = if self.alias.is_some() {
            ck.type_of_symbol(s)
        } else {
            type_at(ck, s, self.at)
        };
        let ts = ck.type_to_string_at(t, self.at);
        self.out.push_str(&drop_optional_undefined(&ts));
    }

    fn enum_member(&mut self, ck: &mut Ck<'_, '_>, s: SymId) {
        self.line();
        let t = ck.type_of_symbol(s);
        let (named, value) = match ck.store().kind(t) {
            TypeKind::EnumLiteral { value, .. } => (true, Some(value)),
            _ => (false, None),
        };
        // The literal's printed name qualifies the enum as the location sees it; a
        // one-member enum prints as the enum itself, so fall back to `Enum.member`.
        let printed = named.then(|| ck.type_to_string_at(t, self.at));
        let name = match printed {
            Some(p) if p.contains('.') || p.contains('[') => p,
            _ => self.name(ck, s),
        };
        self.out.push_str(&format!("(enum member) {name}"));
        if let Some(v) = value {
            let vs = ck.type_to_string(v);
            self.out.push_str(&format!(" = {vs}"));
        }
    }

    fn functions(&mut self, ck: &mut Ck<'_, '_>, s: SymId, is_method: bool) {
        let c = ck.c;
        let at = self.at;
        // On a declaration's own name: that declaration's signature.
        let own = c.parent(at).filter(|&p| {
            let ast = c.ast(p.file);
            matches!(
                ast.node(p.node),
                Node::FunctionDecl { .. }
                    | Node::FunctionExpr { .. }
                    | Node::MethodDef { .. }
                    | Node::MethodSignature { .. }
            ) && decl_name_node(ast, p.node) == Some(at.node)
                && c.decls(s).contains(&p)
        });
        let sigs = match own {
            Some(d) => vec![ck.signature_from_decl(d)],
            None => signatures_at(ck, s, SigKind::Call, at),
        };
        if sigs.is_empty() {
            // A method or function whose type has no signature (`any`): its type.
            self.line();
            let name = self.name(ck, s);
            let t = ck.type_of_symbol(s);
            let ts = ck.type_to_string_at(t, at);
            let p = if is_method { "(method) " } else { "function " };
            self.out.push_str(&format!("{p}{name}: {ts}"));
            return;
        }
        let prefix = if is_method { "(method) " } else { "function " };
        self.signatures(ck, s, &sigs, prefix, SigKind::Call);
    }

    /// One line per signature, `prefix name sig`.
    fn signatures(
        &mut self,
        ck: &mut Ck<'_, '_>,
        s: SymId,
        sigs: &[SigId],
        prefix: &str,
        kind: SigKind,
    ) {
        let c = ck.c;
        let name = self.name(ck, s);
        let opt = if c.sym_flags(s).intersects(SymbolFlags::OPTIONAL) {
            "?"
        } else {
            ""
        };
        for (i, &sig) in sigs.iter().enumerate() {
            self.line();
            if i == 3 && sigs.len() >= 5 {
                self.out
                    .push_str(&format!("// +{} more overloads", sigs.len() - 3));
                break;
            }
            let args = sig_type_args(ck, sig);
            let mut stack = Vec::new();
            let text = ck.print_signature(sig, kind, false, 0, &mut stack);
            let text = text.strip_prefix("new ").unwrap_or(&text);
            let text = drop_optional_undefined(text);
            self.out
                .push_str(&format!("{prefix}{name}{opt}{args}{text}"));
        }
    }

    /// A class's `constructor` member: `constructor C(x: number): C`.
    fn constructor_member(&mut self, ck: &mut Ck<'_, '_>, s: SymId) {
        let c = ck.c;
        let Some(class) = c.sym_parent(s) else {
            return;
        };
        let t = ck.type_of_symbol(class);
        let mut sigs = ck.signatures_of(t, SigKind::Construct);
        // On a `constructor` declaration: the class's construct signature it declares.
        if let Some(p) = c.parent(self.at)
            && c.decls(s).contains(&p)
        {
            let st = ck.store();
            if let Some(&own) = sigs
                .iter()
                .find(|&&g| st.sig(g).decl.and_then(Loc::unpack) == Some(p))
            {
                sigs = vec![own];
            } else if !sigs.is_empty() {
                // The implementation of overloads: its own parameters, the class instance.
                let sig = ck.signature_from_decl(p);
                let mut stack = Vec::new();
                let text = ck.print_signature(sig, SigKind::Call, false, 0, &mut stack);
                let text = text.strip_suffix(": void").unwrap_or(&text).to_string();
                let inst = ck.declared_type_of_symbol(class);
                let inst = ck.type_to_string_at(inst, self.at);
                let name = self.name(ck, class);
                self.line();
                let text = drop_optional_undefined(&text);
                self.out
                    .push_str(&format!("constructor {name}{text}: {inst}"));
                return;
            }
        }
        self.signatures(ck, class, &sigs, "constructor ", SigKind::Construct);
    }

    fn class_or_interface(&mut self, ck: &mut Ck<'_, '_>, s: SymId, flags: SymbolFlags) {
        let c = ck.c;
        if flags.intersects(SymbolFlags::CLASS) && call_of(ck, self.at).is_some() {
            let sigs = signatures_at(ck, s, SigKind::Construct, self.at);
            if sigs.len() == 1 {
                self.signatures(ck, s, &sigs, "constructor ", SigKind::Construct);
                return;
            }
        }
        self.line();
        let name = self.name(ck, s);
        let tps = self.type_params(ck, s);
        if flags.intersects(SymbolFlags::CLASS) {
            let expr = c
                .decls(s)
                .iter()
                .any(|d| matches!(c.node(*d), Node::ClassExpr { .. }));
            if expr {
                self.out.push_str("(local class) ");
            } else {
                if c.sym_facts(s).intersects(SymbolFacts::ABSTRACT) {
                    self.out.push_str("abstract ");
                }
                self.out.push_str("class ");
            }
        } else {
            self.out.push_str("interface ");
        }
        self.out.push_str(&format!("{name}{tps}"));
    }

    fn type_parameter(&mut self, ck: &mut Ck<'_, '_>, s: SymId) {
        let c = ck.c;
        self.line();
        let t = ck.declared_type_of_symbol(s);
        let name = c.sym_name(s).to_string();
        self.out.push_str(&format!("(type parameter) {name}"));
        if let Some(cn) = ck.type_param_constraint(t) {
            let cs = ck.type_to_string_at(cn, self.at);
            self.out.push_str(&format!(" extends {cs}"));
        }
        let Some(d) = c
            .decls(s)
            .into_iter()
            .find(|d| matches!(c.node(*d), Node::TypeParam { .. }))
        else {
            return;
        };
        // TypeParam -> TypeParams -> its declaration.
        let Some(owner) = c.parent(d).and_then(|l| c.parent(l)) else {
            return;
        };
        match *c.node(owner) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. } => {
                if let Some(os) = c.sym_of_decl(owner) {
                    let on = self.name(ck, os);
                    let tps = self.type_params(ck, os);
                    self.out.push_str(&format!(" in {on}{tps}"));
                }
            }
            Node::TypeAlias { .. } => {
                if let Some(os) = c.sym_of_decl(owner) {
                    let on = self.name(ck, os);
                    let tps = self.type_params(ck, os);
                    self.out.push_str(&format!(" in type {on}{tps}"));
                }
            }
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. } => {
                // A method's type parameters hang under its `FunctionExpr` value.
                let decl = match c.parent(owner) {
                    Some(m) if matches!(c.node(m), Node::MethodDef { .. }) => m,
                    _ => owner,
                };
                self.out.push_str(" in ");
                // tsc writes a construct signature's owner as `new ` and then the signature,
                // which itself starts with `new ` (`in new new <U>(a: U): U`).
                let kind = match *c.node(decl) {
                    Node::ConstructSignature { .. } | Node::ConstructorType { .. } => {
                        SigKind::Construct
                    }
                    _ => SigKind::Call,
                };
                match *c.node(decl) {
                    Node::ConstructSignature { .. } => self.out.push_str("new "),
                    Node::CallSignature { .. } | Node::FunctionType { .. } => {}
                    Node::ConstructorType { .. } => {}
                    _ => {
                        if decl_name_node(c.ast(decl.file), decl.node).is_some()
                            && let Some(ds) = c.sym_of_decl(decl)
                        {
                            let dn = self.name(ck, ds);
                            self.out.push_str(&dn);
                        }
                    }
                }
                let sig = ck.signature_from_decl(owner);
                let mut stack = Vec::new();
                let text = ck.print_signature(sig, kind, false, 0, &mut stack);
                self.out.push_str(&drop_optional_undefined(&text));
            }
            _ => {}
        }
    }

    /// `<T extends C = D, U>` for a class, interface or type alias's own type parameters.
    fn type_params(&mut self, ck: &mut Ck<'_, '_>, s: SymId) -> String {
        let tps = ck.local_type_params(s);
        if tps.is_empty() {
            return String::new();
        }
        let mut parts = Vec::with_capacity(tps.len());
        for tp in tps {
            let mut p = ck.type_to_string(tp);
            if let Some(cn) = ck.type_param_constraint(tp) {
                p.push_str(" extends ");
                p.push_str(&ck.type_to_string_at(cn, self.at));
            }
            if let Some(d) = ck.type_param_default(tp) {
                p.push_str(" = ");
                p.push_str(&ck.type_to_string_at(d, self.at));
            }
            parts.push(p);
        }
        format!("<{}>", parts.join(", "))
    }

    /// The name of `s` as quick info writes it: owner first for members (`C.m`, `G<T>.p`),
    /// then the namespaces that do not enclose the location. Through an alias whose target
    /// cannot be named at the location, the alias's own name.
    fn name(&mut self, ck: &mut Ck<'_, '_>, s: SymId) -> String {
        let c = ck.c;
        let base = c.sym_name(s).to_string();
        if let Some(a) = self.alias
            && !reachable(ck, s, self.at)
        {
            return c.sym_name(a).to_string();
        }
        let flags = c.sym_flags(s);
        let member = flags.intersects(
            SymbolFlags::PROPERTY
                | SymbolFlags::METHOD
                | SymbolFlags::ACCESSOR
                | SymbolFlags::ENUM_MEMBER
                | SymbolFlags::CONSTRUCTOR,
        );
        let mut out = base;
        let mut cur = c.sym_parent(s);
        if member {
            match owner(ck, s) {
                Some(o) => {
                    let on = c.sym_name(o).to_string();
                    let ofl = c.sym_flags(o);
                    let tps = if ofl.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE) {
                        self.owner_type_args(ck, s, o)
                    } else {
                        String::new()
                    };
                    out = format!("{on}{tps}{}", member_key(ck, s, &out));
                    cur = c.sym_parent(o);
                }
                None => return out,
            }
        }
        if self.is_decl {
            return out;
        }
        // Enclosing namespaces, up to one whose body holds the location.
        for _ in 0..32 {
            let Some(p) = cur else { break };
            let f = c.sym_flags(p);
            if f.intersects(SymbolFlags::MODULE_FILE)
                || !f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
                || c.sym_name(p) == "__global"
                || encloses(ck, p, self.at)
            {
                break;
            }
            out = format!("{}.{out}", c.sym_name(p));
            cur = c.sym_parent(p);
        }
        out
    }

    /// The type parameters of a generic member's owner: the type arguments when the member
    /// was instantiated and instantiation changed its type (`c<string>.method`), the
    /// declared parameters otherwise (`Array<T>.length`, `c2<T extends c<string>>.m`).
    fn owner_type_args(&mut self, ck: &mut Ck<'_, '_>, s: SymId, o: SymId) -> String {
        let c = ck.c;
        let inst = c.synth_sym(s).and_then(|x| {
            let origin = *x.origins.first()?;
            (x.mapper != MapperId::IDENTITY && origin != s).then_some((origin, x.mapper))
        });
        if let Some((origin, mapper)) = inst {
            // tsc keeps the declared member when instantiation cannot change its type;
            // the printed types tell us whether it did.
            // Methods are always instantiated (their function type may mention the
            // owner's type parameters); other members only when their type changes.
            let now = ck.type_of_symbol(s);
            let was = ck.type_of_symbol(origin);
            let method = ck.c.sym_flags(origin).intersects(SymbolFlags::METHOD);
            let changed =
                now != was && (method || ck.type_to_string(now) != ck.type_to_string(was));
            let tps = ck.local_type_params(o);
            if changed && !tps.is_empty() {
                let mut parts = Vec::with_capacity(tps.len());
                for tp in tps {
                    let a = ck.instantiate(tp, mapper);
                    parts.push(ck.type_to_string_at(a, self.at));
                }
                return format!("<{}>", parts.join(", "));
            }
        }
        self.type_params(ck, o)
    }
}

/// `.name`, or `["name"]` / `[1]` for a member declared with a literal name.
fn member_key(ck: &Ck<'_, '_>, s: SymId, name: &str) -> String {
    let c = ck.c;
    if let Some(d) = c.first_decl(s) {
        let ast = c.ast(d.file);
        if let Some(n) = decl_name_node(ast, d.node) {
            match ast.node(n) {
                Node::StringLit { .. } => return format!("[\"{name}\"]"),
                Node::NumberLit { .. } => return format!("[{name}]"),
                _ => {}
            }
        }
    }
    format!(".{name}")
}

/// The class, interface or enum a member is declared in (`None` for object literal and type
/// literal members).
fn owner(ck: &Ck<'_, '_>, s: SymId) -> Option<SymId> {
    let c = ck.c;
    if let Some(p) = c.sym_parent(s) {
        let n = c.sym_name(p);
        if !n.is_empty() && !n.starts_with("__") {
            return Some(p);
        }
        return None;
    }
    // Instantiated members have no binder parent: walk up from the declaration.
    let d = c.first_decl(s)?;
    let mut cur = c.parent(d);
    for _ in 0..64 {
        let l = cur?;
        match *c.node(l) {
            Node::ClassDecl { name: Some(_), .. }
            | Node::ClassExpr { name: Some(_), .. }
            | Node::Interface { .. }
            | Node::Enum { .. } => return c.sym_of_decl(l),
            Node::TypeLiteral { .. } | Node::ObjectLit { .. } => return None,
            _ => cur = c.parent(l),
        }
    }
    None
}

/// Whether a declaration of namespace `p` holds `at`.
fn encloses(ck: &Ck<'_, '_>, p: SymId, at: Loc) -> bool {
    let c = ck.c;
    let span = c.span(at);
    c.decls(p).iter().any(|d| {
        d.file == at.file && matches!(c.node(*d), Node::Module { .. } | Node::Enum { .. }) && {
            let ds = c.span(*d);
            ds.start <= span.start && span.end <= ds.end
        }
    })
}

/// Whether `s` can be named at `at` without an import: its outermost namespace is a global
/// or lives in `at`'s file.
fn reachable(ck: &Ck<'_, '_>, s: SymId, at: Loc) -> bool {
    let c = ck.c;
    // Declared inside an ambient `declare module "m"`: only reachable through an import.
    if let Some(d) = c.first_decl(s) {
        let mut up = c.parent(d);
        for _ in 0..64 {
            let Some(l) = up else { break };
            if let Node::Module { name, .. } = *c.node(l)
                && matches!(c.ast(l.file).node(name), Node::StringLit { .. })
            {
                return false;
            }
            up = c.parent(l);
        }
    }
    let mut cur = s;
    for _ in 0..32 {
        let Some(p) = c.sym_parent(cur) else {
            return true;
        };
        let f = c.sym_flags(p);
        if f.intersects(SymbolFlags::MODULE_FILE) || c.sym_name(p).starts_with('"') {
            // A module file, or an ambient `declare module "m"`: only names in the
            // location's own (non-ambient) file are in scope.
            return c.first_decl(p).is_some_and(|d| d.file == at.file)
                && !c.sym_name(p).starts_with('"');
        }
        if !f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE) {
            // A member: reachable through its owner.
            cur = p;
            continue;
        }
        cur = p;
    }
    true
}

/// `<string, number>`: the type arguments an instantiated generic signature was made with.
fn sig_type_args(ck: &mut Ck<'_, '_>, sig: SigId) -> String {
    let st = ck.store();
    let d = st.sig(sig);
    if !st.list_items(d.type_params).is_empty() {
        return String::new();
    }
    let Some((orig, mapper)) = d.target else {
        return String::new();
    };
    let tps = st.list_items(st.sig(orig).type_params).to_vec();
    if tps.is_empty() {
        return String::new();
    }
    let mut parts = Vec::with_capacity(tps.len());
    for tp in tps {
        let a = ck.instantiate(tp, mapper);
        parts.push(ck.type_to_string(a));
    }
    format!("<{}>", parts.join(", "))
}

/// The signatures to show for `s` at `at`: at a call of an overloaded or generic function,
/// the one the call resolved to; otherwise all of them.
fn signatures_at(ck: &mut Ck<'_, '_>, s: SymId, kind: SigKind, at: Loc) -> Vec<SigId> {
    let t = ck.type_of_symbol(s);
    let t = ck.remove_missing(t);
    let sigs = ck.signatures_of(t, kind);
    let generic = sigs.len() == 1 && {
        let st = ck.store();
        !st.list_items(st.sig(sigs[0]).type_params).is_empty()
    };
    if (sigs.len() > 1 || generic)
        && let Some(call) = call_of(ck, at)
        && let Some(r) = ck.resolved_signature(call)
    {
        return vec![r];
    }
    sigs
}

/// The type of `s` as seen at `at`: narrowed for expression names, declared otherwise.
pub fn type_at(ck: &mut Ck<'_, '_>, s: SymId, at: Loc) -> TypeId {
    let c = ck.c;
    if let Some(p) = c.parent(at)
        && let Node::Member { prop, optional, .. } = *c.node(p)
        && prop == at.node
        && c.parent(p).is_some()
    {
        let t = ck.check_expr(p);
        // The `?.` access itself (`a?.b`) shows the property's type without the
        // `undefined` the chain adds; later links (`a?.b.c`) keep it, as in tsc.
        let own = ck.type_of_symbol(s);
        if optional
            && !ck.c.sym_flags(s).intersects(SymbolFlags::OPTIONAL)
            && ck.non_nullable(own) == own
        {
            return ck.non_nullable(t);
        }
        return t;
    }
    if super::locate::is_expression_name(ck, at) && matches!(c.node(at), Node::Ident { .. }) {
        return ck.check_expr(at);
    }
    ck.type_of_symbol(s)
}

/// `(parameter) `, `const `, `let `, `var `, `using `, `await using ` from the variable's
/// root declaration.
fn var_prefix(ck: &Ck<'_, '_>, s: SymId) -> &'static str {
    let c = ck.c;
    if c.sym_flags(s).intersects(SymbolFlags::PARAMETER) {
        return "(parameter) ";
    }
    let Some(d) = c.value_decl(s).or_else(|| c.first_decl(s)) else {
        return "var ";
    };
    let mut cur = Some(d);
    for _ in 0..32 {
        let Some(l) = cur else { break };
        match *c.node(l) {
            Node::VarDecl { kind, .. } => {
                return match kind {
                    VarKind::Var => "var ",
                    VarKind::Let => "let ",
                    VarKind::Const => "const ",
                    VarKind::Using => "using ",
                    VarKind::AwaitUsing => "await using ",
                };
            }
            Node::Param { .. } => return "(parameter) ",
            Node::CatchClause { .. } => return "var ",
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::Program { .. } => break,
            _ => cur = c.parent(l),
        }
    }
    if c.sym_flags(s)
        .intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
    {
        "let "
    } else {
        "var "
    }
}

/// The aliased type of a type alias, as written (whitespace collapsed).
fn alias_body(ck: &mut Ck<'_, '_>, s: SymId) -> String {
    let c = ck.c;
    let Some(d) = c
        .decls(s)
        .into_iter()
        .find(|d| matches!(c.node(*d), Node::TypeAlias { .. }))
    else {
        let t = ck.declared_type_of_symbol(s);
        return ck.type_to_string(t);
    };
    let Node::TypeAlias { ty, .. } = *c.node(d) else {
        return String::new();
    };
    super::hover::collapse_ws(c.text(Loc::new(d.file, ty)))
}

pub fn strip_ts_ext(path: &str) -> &str {
    for ext in [
        ".d.ts", ".d.mts", ".d.cts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs",
    ] {
        if let Some(s) = path.strip_suffix(ext) {
            return s;
        }
    }
    path
}

/// Quick info writes optional parameters and properties without the `undefined` their `?`
/// adds (`c?: number`, `func?: Function`), where diagnostics keep it. Applied to printed
/// text: each `?: T | undefined` that ends a parameter or member loses ` | undefined`, and a
/// function type left in parentheses loses them (`c?: () => string`).
pub fn drop_optional_undefined(s: &str) -> String {
    const UNDEF: &str = " | undefined";
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' | b'`' => {
                i = skip_quoted(b, i);
                continue;
            }
            b'?' if s[i..].starts_with("?: ") => {
                let start = i + 3;
                let end = type_end(b, start);
                let ty = &s[start..end];
                if let Some(inner) = ty.strip_suffix(UNDEF) {
                    out.push_str(&s[copied..start]);
                    out.push_str(unparen(inner));
                    copied = end;
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

fn skip_quoted(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == q => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// Where the type starting at `i` ends: the first `,`, `;`, `)`, `]`, `}`, `>` or newline
/// outside its own brackets (`=>` is not a bracket).
fn type_end(b: &[u8], mut i: usize) -> usize {
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' | b'`' => {
                i = skip_quoted(b, i);
                continue;
            }
            b'=' if b.get(i + 1) == Some(&b'>') => i += 1,
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            b',' | b';' | b'\n' if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// `(X)` without its parentheses when they wrap the whole text.
fn unparen(t: &str) -> &str {
    let Some(inner) = t.strip_prefix('(').and_then(|r| r.strip_suffix(')')) else {
        return t;
    };
    let mut depth = 0i32;
    for ch in inner.bytes() {
        match ch {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return t;
                }
            }
            _ => {}
        }
    }
    if depth == 0 { inner } else { t }
}

#[cfg(test)]
mod tests {
    use super::drop_optional_undefined as d;

    #[test]
    fn optional_members_lose_their_undefined() {
        assert_eq!(
            d("function f(a: number, c?: number | undefined, d?: any): void"),
            "function f(a: number, c?: number, d?: any): void"
        );
        assert_eq!(
            d("function g(c?: (() => string) | undefined): void"),
            "function g(c?: () => string): void"
        );
        assert_eq!(
            d("(a: T) => { v: T[U]; func?: Function | undefined; }"),
            "(a: T) => { v: T[U]; func?: Function; }"
        );
        assert_eq!(
            d("f(x?: Map<string, number> | undefined)"),
            "f(x?: Map<string, number>)"
        );
        // A required member and a union that only contains `undefined` stay.
        assert_eq!(d("(x: number | undefined)"), "(x: number | undefined)");
        assert_eq!(
            d("(x?: (string | undefined)[])"),
            "(x?: (string | undefined)[])"
        );
        assert_eq!(
            d("(x?: \"?: a | undefined\")"),
            "(x?: \"?: a | undefined\")"
        );
    }
}
