//! Queries a program emitter asks (design record 0012, `EmitResolver`). `ts_r_emit` adapts
//! them to `ts_r_transform::EmitResolver`; the checker does not depend on the transform.

use ts_r_ast::Node;
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_types::TypeKind;

use crate::checker::{Checker, Ck, Loc};

impl Checker<'_> {
    /// Whether the alias declared at `decl` (an import or export specifier, a default or
    /// namespace import, `import x = ...`) stands for something with a value meaning, so its
    /// statement stays in the JavaScript output. `Some(true)` for unresolvable targets (tsc
    /// keeps an import of a missing module). `None` when the checker cannot tell (no symbol
    /// for `decl`, or a const enum whose values cannot be evaluated). A const enum without
    /// `preserveConstEnums` is not a value: its uses are inlined
    /// ([`Checker::const_enum_access_value`]).
    ///
    /// `preserve_const_enums` is the program's effective `preserveConstEnums`.
    pub fn alias_is_value(&self, decl: Loc, preserve_const_enums: bool) -> Option<bool> {
        let alias = self.sym_of_decl(decl)?;
        let Some(target) = self.resolve_alias(alias) else {
            return Some(true);
        };
        let flags = self.sym_flags(target);
        let other_values = SymbolFlags::VARIABLE
            | SymbolFlags::FUNCTION
            | SymbolFlags::CLASS
            | SymbolFlags::REGULAR_ENUM
            | SymbolFlags::VALUE_MODULE;
        if flags.intersects(SymbolFlags::CONST_ENUM)
            && !flags.intersects(other_values)
            && !preserve_const_enums
        {
            let mut ck = Ck::new(self, decl.file);
            ck.no_report += 1;
            if ck
                .enum_members(target)
                .iter()
                .any(|(_, value)| value.is_none())
            {
                return None;
            }
            return Some(false);
        }
        if !self.has_meaning(target, meaning::VALUE) && self.default_export_has_value(alias) {
            return Some(true);
        }
        Some(self.has_meaning(target, meaning::VALUE))
    }

    /// Whether the alias chain from `alias` passes an `export default X` whose `X` is both a
    /// local type and an exported value of its file (`interface F {}` with `export const
    /// F`): the binder resolved `X` to the local type alone, but the default export stands
    /// for both (importElisionExportNonExportAndDefault; tsc merges the two in the
    /// module's locals).
    fn default_export_has_value(&self, alias: ts_r_types::SymId) -> bool {
        let Some(r) = self.parts(alias).next() else {
            return false;
        };
        for step in self.link.alias_chain(r) {
            for d in self.decls(self.sym_ref(step)) {
                let ast = self.ast(d.file);
                let Node::ExportDefaultExpr { expr } = *ast.node(d.node) else {
                    continue;
                };
                let Node::Ident { name } = *ast.node(expr) else {
                    continue;
                };
                let bound = &self.file(d.file).bound;
                let module = self.sym(d.file, bound.file_symbol);
                if let Some(v) = self.export_of(module, ast.atom(name))
                    && self.has_meaning(v, meaning::VALUE)
                {
                    return true;
                }
            }
        }
        false
    }

    /// The value of `access` (`E.A`, `E["A"]`, `ns.E.A`) when `E` is a const enum (of any
    /// file, through aliases): what tsc inlines in the JavaScript output.
    pub fn const_enum_access_value(&self, access: Loc) -> Option<EnumConst> {
        let ast = self.ast(access.file);
        let (obj, key) = match *ast.node(access.node) {
            Node::Member { obj, prop, .. } => {
                (obj, crate::nodes::ident_text(ast, prop)?.to_owned())
            }
            Node::Index { obj, index, .. } => match *ast.node(index) {
                Node::StringLit { value } => (obj, ast.atom(value).to_owned()),
                Node::TemplateLit { parts } => match *ast.list(parts) {
                    [only] => match *ast.node(only) {
                        Node::TemplateElement { cooked, .. } => (obj, ast.atom(cooked).to_owned()),
                        _ => return None,
                    },
                    _ => return None,
                },
                _ => return None,
            },
            _ => return None,
        };
        let mut ck = Ck::new(self, access.file);
        ck.no_report += 1;
        let e = ck.resolve_entity(
            Loc::new(access.file, obj),
            meaning::VALUE | meaning::NAMESPACE,
        )?;
        let e = self.resolve_alias(e)?;
        if !self.sym_flags(e).intersects(SymbolFlags::CONST_ENUM) {
            return None;
        }
        let m = self.export_of(e, &key)?;
        self.cached_enum_member_value(&mut ck, e, m)
    }

    /// The value the checker's evaluator gives the enum member declared at `member` (an
    /// `EnumMember` node of any enum, const or not): initializers that reach other files
    /// (imported consts, template literals over them, members of other enums) included.
    /// `None` when the member is computed (no constant value).
    pub fn enum_member_value(&self, member: Loc) -> Option<EnumConst> {
        if !matches!(
            self.ast(member.file).node(member.node),
            Node::EnumMember { .. }
        ) {
            return None;
        }
        let m = self.sym_of_decl(member)?;
        let e = self.sym_parent(m)?;
        if let Some(v) = self.cached.enum_member_values.lock().ok()?.get(&m) {
            return v.clone();
        }
        let mut ck = Ck::new(self, member.file);
        ck.no_report += 1;
        self.cached_enum_member_value(&mut ck, e, m)
    }

    /// The value of member `m` of enum `e`: from the cache, else every member of `e`
    /// evaluated once into it (constant per member, not per query: emitting an enum of
    /// many members whose initializers do not fold reads its member list once).
    fn cached_enum_member_value(
        &self,
        ck: &mut Ck<'_, '_>,
        e: ts_r_types::SymId,
        m: ts_r_types::SymId,
    ) -> Option<EnumConst> {
        let cache = &self.cached.enum_member_values;
        if let Some(v) = cache.lock().ok()?.get(&m) {
            return v.clone();
        }
        let members = ck.enum_members(e);
        if members.is_empty() {
            // Nothing evaluated (an enum without members, or one met in a cycle): no value,
            // and nothing to keep.
            return None;
        }
        let values: Vec<(ts_r_types::SymId, Option<EnumConst>)> = members
            .iter()
            .map(|&(s, v)| (s, v.and_then(|v| enum_const_of(ck, v))))
            .collect();
        let mut cache = cache.lock().ok()?;
        let mut found = None;
        for (s, v) in values {
            if s == m {
                found = Some(v.clone());
            }
            cache.entry(s).or_insert(v);
        }
        let found = found.flatten();
        cache.entry(m).or_insert_with(|| found.clone());
        found
    }
}

/// A literal enum member type as its constant.
fn enum_const_of(ck: &Ck<'_, '_>, v: ts_r_types::TypeId) -> Option<EnumConst> {
    let s = ck.store();
    match s.kind(v) {
        TypeKind::StringLiteral(x) => Some(EnumConst::String(s.str(x).to_owned())),
        TypeKind::NumberLiteral(..) => s.number_value(v).map(EnumConst::Number),
        _ => None,
    }
}

/// A const enum member's value ([`Checker::const_enum_access_value`]).
#[derive(Clone, Debug, PartialEq)]
pub enum EnumConst {
    Number(f64),
    String(String),
}
