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
        Some(self.has_meaning(target, meaning::VALUE))
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
        let v = ck.enum_members(e).into_iter().find(|x| x.0 == m)?.1?;
        enum_const_of(&ck, v)
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
        let mut ck = Ck::new(self, member.file);
        ck.no_report += 1;
        let v = ck.enum_members(e).into_iter().find(|x| x.0 == m)?.1?;
        enum_const_of(&ck, v)
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
