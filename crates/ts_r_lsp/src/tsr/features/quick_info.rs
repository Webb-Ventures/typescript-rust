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

use ts_r_ast::{MethodKind, ModuleKind, Node, VarKind};
use ts_r_binder::{SymbolFacts, SymbolFlags, decl_name_node, meaning};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc, PrintMode};
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
    /// How quick info prints types and signatures here.
    fn mode(&self) -> PrintMode {
        PrintMode::quick_info(self.at)
    }

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
                // A file module: the specifier this file would import it with.
                let spec = ck
                    .module_specifier_for(self.at.file, s, None)
                    .unwrap_or_else(|| strip_ts_ext(&path).to_string());
                self.out.push_str(&format!("module \"{spec}\""));
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
            let body = alias_body(ck, s, PrintMode::quick_info_alias(self.at));
            self.out.push_str(&format!("type {name}{tps} = {body}"));
        }
        if flags.intersects(SymbolFlags::SIGNATURE) {
            self.line();
            let t = ck.type_of_symbol(s);
            let ts = ck.type_to_string_with(t, self.mode());
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
        // tsc names the module by the first `import x = require(..)` of it in this file.
        let name = c
            .resolve_alias(s)
            .filter(|_| equals)
            .and_then(|m| first_equals_alias(ck, self.at.file, m))
            .unwrap_or_else(|| c.sym_name(s).to_string());
        self.out.push_str(&format!("{kw} {name}"));
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
            let text = ck.signature_to_string_with(sig, kind, true, self.mode());
            let text = match (args.is_empty(), text.strip_prefix("new ")) {
                (false, Some(rest)) => format!("new {args}{rest}"),
                (false, None) => format!("{args}{text}"),
                _ => text,
            };
            self.out.push_str(&text);
            return;
        }
        // Through an alias, tsc shows the target's declared type (no narrowing); a
        // property being assigned shows the type it accepts (no `undefined` from an
        // `exactOptionalPropertyTypes` `?`).
        let t = if self.alias.is_some() {
            ck.type_of_symbol(s)
        } else if flags.intersects(SymbolFlags::ACCESSOR)
            && (assigned_member(ck, self.at) || is_setter_name(ck, self.at))
        {
            // A setter's name or a write through it: the type it accepts.
            ck.write_type_of_symbol(s)
        } else if flags.intersects(SymbolFlags::PROPERTY) && assigned_member(ck, self.at) {
            let t = ck.type_of_symbol(s);
            ck.remove_missing(t)
        } else {
            type_at(ck, s, self.at)
        };
        let ts = ck.type_to_string_with(t, self.mode());
        self.out.push_str(&ts);
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
        let printed = named.then(|| ck.type_to_string_with(t, self.mode()));
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
            let ts = ck.type_to_string_with(t, self.mode());
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
            let text = ck.signature_to_string_with(sig, kind, false, self.mode());
            let text = text.strip_prefix("new ").unwrap_or(&text);
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
                let text = ck.signature_to_string_with(sig, SigKind::Call, false, self.mode());
                let text = text.strip_suffix(": void").unwrap_or(&text).to_string();
                let inst = ck.declared_type_of_symbol(class);
                let inst = ck.type_to_string_with(inst, self.mode());
                let name = self.name(ck, class);
                self.line();
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
            let cs = ck.type_to_string_with(cn, self.mode());
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
                let text = ck.signature_to_string_with(sig, kind, false, self.mode());
                self.out.push_str(&text);
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
                p.push_str(&ck.type_to_string_with(cn, self.mode()));
            }
            if let Some(d) = ck.type_param_default(tp) {
                p.push_str(" = ");
                p.push_str(&ck.type_to_string_with(d, self.mode()));
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
        let base = shown_name(ck, s);
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
            match owner(ck, s).or_else(|| prototype_owner(ck, s)) {
                Some(o) => {
                    let on = shown_name(ck, o);
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
            // Another file's module, imported here with `import x = require(..)`: `x.`.
            if f.intersects(SymbolFlags::MODULE_FILE) {
                if let Some(a) = first_equals_alias(ck, self.at.file, p) {
                    out = format!("{a}.{out}");
                }
                break;
            }
            if !f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
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
                    parts.push(ck.type_to_string_with(a, self.mode()));
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
pub(super) fn signatures_at(ck: &mut Ck<'_, '_>, s: SymId, kind: SigKind, at: Loc) -> Vec<SigId> {
    // An optional method's type carries the `undefined` optionality adds.
    let t = ck.type_of_symbol(s);
    let t = ck.remove_optionality(t);
    let sigs = ck.signatures_of(t, kind);
    let generic = sigs.len() == 1 && {
        let st = ck.store();
        !st.list_items(st.sig(sigs[0]).type_params).is_empty()
    };
    if (sigs.len() > 1 || generic)
        && let Some(call) = call_of(ck, at)
    {
        let resolved = ck.resolved_signature(call);
        let still_generic = resolved.is_some_and(|r| {
            let st = ck.store();
            !st.list_items(st.sig(r).type_params).is_empty()
        });
        if let Some(r) = resolved
            && !still_generic
        {
            return vec![r];
        }
        // A generic call that does not resolve (no signature, or the candidate left
        // uninstantiated) shows that candidate instantiated as signature help does
        // (written type arguments, else inferred; `any` in JavaScript).
        let cand = resolved.unwrap_or(sigs[0]);
        if generic || still_generic {
            let (type_args, args) = match *ck.c.node(call) {
                Node::Call {
                    type_args, args, ..
                }
                | Node::New {
                    type_args, args, ..
                } => (type_args, args),
                _ => return sigs,
            };
            let args: Vec<(usize, ts_r_ast::NodeId)> =
                ck.c.ast(call.file)
                    .list(args)
                    .iter()
                    .copied()
                    .enumerate()
                    .collect();
            let sig =
                super::signature::instantiate_for_failure(ck, call.file, cand, type_args, &args);
            return vec![sig];
        }
        if let Some(r) = resolved {
            return vec![r];
        }
    }
    sigs
}

/// Whether `at` is the name of a `set` accessor declaration.
fn is_setter_name(ck: &Ck<'_, '_>, at: Loc) -> bool {
    let c = ck.c;
    c.parent(at).is_some_and(|p| {
        matches!(*c.node(p),
            Node::MethodDef { kind: MethodKind::Set, key, .. }
            | Node::MethodSignature { kind: MethodKind::Set, key, .. } if key == at.node)
    })
}

/// Whether `at` names the member an assignment writes (`o.p = 1`, `o["p"] ??= 1`, `o.p++`).
fn assigned_member(ck: &Ck<'_, '_>, at: Loc) -> bool {
    let c = ck.c;
    let Some(access) = c.parent(at) else {
        return false;
    };
    let named = match *c.node(access) {
        Node::Member { prop, .. } => prop == at.node,
        Node::Index { index, .. } => index == at.node,
        _ => false,
    };
    if !named {
        return false;
    }
    match c.parent(access).map(|p| *c.node(p)) {
        Some(Node::Assign { left, .. }) => left == access.node,
        Some(Node::Update { .. }) => true,
        _ => false,
    }
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

/// The aliased type of a type alias, printed as tsc's quick info prints it: the declared
/// type with its own alias expanded (`PrintMode::quick_info_alias`, tsc's `InTypeAlias`),
/// so `Exclude<A, "_0">` shows the union it makes. A conditional type shows its text as
/// written (whitespace collapsed): tsc keeps one with a generic check type deferred, which
/// the checker does not yet.
fn alias_body(ck: &mut Ck<'_, '_>, s: SymId, mode: PrintMode) -> String {
    let c = ck.c;
    let t = ck.declared_type_of_symbol(s);
    if let Some(d) = c
        .decls(s)
        .into_iter()
        .find(|d| matches!(c.node(*d), Node::TypeAlias { .. }))
        && let Node::TypeAlias { ty, .. } = *c.node(d)
        && matches!(c.ast(d.file).node(ty), Node::ConditionalType { .. })
    {
        return super::hover::collapse_ws(c.text(Loc::new(d.file, ty)));
    }
    ck.type_to_string_with(t, mode)
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

/// The name of the first `import x = require("..")` in `file` (top level or a namespace
/// body) that stands for module `m`.
pub(super) fn first_equals_alias(
    ck: &Ck<'_, '_>,
    file: ts_r_diagnostics::FileId,
    m: SymId,
) -> Option<String> {
    let c = ck.c;
    let ast = c.ast(file);
    let mut stack = vec![(ast.root, 0u32)];
    let mut found: Option<(u32, String)> = None;
    while let Some((n, depth)) = stack.pop() {
        match *ast.node(n) {
            Node::Program { stmts } | Node::ModuleBlock { stmts } => {
                for &st in ast.list(stmts).iter().rev() {
                    stack.push((st, depth + 1));
                }
            }
            Node::Module { body: Some(b), .. } if depth < 32 => stack.push((b, depth + 1)),
            Node::ImportEquals {
                name, module_ref, ..
            } if matches!(ast.node(module_ref), Node::ExternalModuleRef { .. }) => {
                let start = ast.span(n).start;
                if found.as_ref().is_some_and(|(s, _)| *s <= start) {
                    continue;
                }
                if let Some(x) = c.sym_of_decl(Loc::new(file, n))
                    && c.resolve_alias(x) == Some(m)
                    && let Some(t) = super::locate::name_text(ast, name)
                {
                    found = Some((start, t.to_string()));
                }
            }
            _ => {}
        }
    }
    found.map(|(_, n)| n)
}

/// A symbol's name as quick info writes it: an anonymous class or function expression is
/// `(Anonymous class)` / `(Anonymous function)` (a function initializing a variable takes
/// its name), a class statement missing its name `__missing`.
fn shown_name(ck: &Ck<'_, '_>, s: SymId) -> String {
    let c = ck.c;
    let name = c.sym_name(s);
    match name {
        "__class" => {
            let missing = c.decls(s).iter().any(|&d| {
                matches!(*c.node(d), Node::ClassDecl { name: None, modifiers, .. }
                    if !modifiers.contains(ts_r_ast::Modifiers::DEFAULT))
            });
            if missing {
                "__missing"
            } else {
                "(Anonymous class)"
            }
            .to_string()
        }
        // A function expression initializing a variable is named by it.
        "__function" => c
            .first_decl(s)
            .and_then(|d| c.parent(d))
            .and_then(|p| match *c.node(p) {
                Node::VarDeclarator { name, .. } => {
                    super::locate::name_text(c.ast(p.file), name).map(str::to_string)
                }
                _ => None,
            })
            .unwrap_or_else(|| "(Anonymous function)".to_string()),
        _ => name.to_string(),
    }
}

/// The class whose `prototype` property `s` is (the property has no declaration).
fn prototype_owner(ck: &mut Ck<'_, '_>, s: SymId) -> Option<SymId> {
    let c = ck.c;
    if c.sym_name(s) != "prototype" || c.first_decl(s).is_some() {
        return None;
    }
    let t = ck.type_of_symbol(s);
    let k = super::nav::type_symbol(ck, t)?;
    ck.c.sym_flags(k)
        .intersects(SymbolFlags::CLASS)
        .then_some(k)
}
