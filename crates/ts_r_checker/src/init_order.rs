//! Class properties used before their initialization (TS2729).
//!
//! Rules observed from tsc 7.0.2. They cover a property access evaluated while a class's
//! property initializer or static block runs, or a field's decorators. Functions, arrows
//! and the computed names of class methods and accessors run later and are skipped.
//! Computed property names, the computed names of object literal methods and accessors,
//! and `extends` clauses of nested class expressions run immediately (without native
//! fields an object literal method or accessor name only reports the member being
//! initialized). For a field of that same class (`accessor` and abstract fields included):
//! - `this.x` (instance initializers read instance fields, static ones static fields):
//!   - an error when `x` is declared at or after the current member;
//!   - also when it has no initializer and is neither optional nor `!`-asserted
//!     (`declare` fields included), unless an earlier static block may assign it. That
//!     second case applies to reads only.
//!   - Parameter properties count as uninitialized when class fields are native
//!     (`useDefineForClassFields` with target ES2022 or later).
//! - Another object (`C.x`, `new C().x`; not a property chain such as `this.p.x`): an error
//!   only when `x` is declared at or after the current member. An instance initializer may
//!   read a later static field unless class fields are native.
//! - Without native fields, a field redeclaring a base class member is not reported.
//! - Methods and accessors are never reported.
//!
//! Static initializers, static blocks and field decorators also report reads of things
//! declared after the read in the same file: static fields of other classes, enum members,
//! object literal properties and namespace exports (`Enum.A`, `Obj.a`, `NS.f`, `NS.Inner`).
//! A JSX closing tag does not repeat its opening tag's error.

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId};
use ts_r_config::ScriptTarget;
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::skip_parens;

/// The class member whose initializer is being evaluated.
struct Container {
    member: NodeId,
    class: NodeId,
    is_static: bool,
    /// Reached through the computed name of an object literal method or accessor.
    through_method_name: bool,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2729 for the property access `e` (`obj.name`), whose property resolved to `p`.
    pub(crate) fn check_property_initialization_order(
        &mut self,
        e: Loc,
        obj: NodeId,
        pl: Loc,
        p: SymId,
        name: &str,
        is_write: bool,
    ) {
        if !self.reporting(e.file) || self.c.file(e.file).is_js() {
            return;
        }
        let ast = self.ast(e.file);
        let obj = skip_parens(ast, obj);
        let is_this = matches!(ast.node(obj), Node::This {});
        if matches!(ast.node(obj), Node::Member { .. }) {
            return;
        }
        // An object literal property is synthesized from its declaration alone.
        let d = match self.c.synth_sym(p) {
            Some(x) if x.origins.len() == 1 => {
                let decls = self.c.decls(x.origins[0]);
                let [d] = decls.as_slice() else {
                    return;
                };
                *d
            }
            Some(x) if x.origins.is_empty() && x.decl.is_some() => match x.decl {
                Some(d) => d,
                None => return,
            },
            Some(_) => return,
            None => {
                let decls = self.c.decls(p);
                let [d] = decls.as_slice() else {
                    return;
                };
                *d
            }
        };
        if d.file != e.file {
            return;
        }
        let parents = self.c.file(e.file).parents();
        let Some(c) = self.initializer_container(e) else {
            return;
        };
        let native_fields =
            self.c.opts.use_define_for_class_fields && self.c.opts.target >= ScriptTarget::ES2022;
        // Without native fields, an object literal method or accessor name only reports
        // the member being initialized.
        if c.through_method_name && !native_fields && d.node != c.member {
            return;
        }
        let report = match *ast.node(d.node) {
            Node::PropertyDef {
                modifiers,
                optional,
                definite,
                value,
                ..
            } => {
                let decl_static = modifiers.contains(Modifiers::STATIC);
                if parents.parent(d.node) != Some(c.class) {
                    // A static field of another class, read by a static initializer.
                    if is_this || !decl_static || !c.is_static {
                        return;
                    }
                    if ast.span(d.node).start >= ast.span(e.node).start
                        && !self.redeclares_base_member(Loc::new(e.file, c.class), name)
                    {
                        self.error(
                            pl,
                            dg::PROPERTY_0_IS_USED_BEFORE_ITS_INITIALIZATION_2729,
                            &[name],
                        );
                    }
                    return;
                }
                let later = ast.span(d.node).start >= ast.span(c.member).start;
                if is_this {
                    decl_static == c.is_static
                        && (later
                            || (!is_write
                                && value.is_none()
                                && !optional
                                && !definite
                                && !self.static_block_before(e.file, c.class, c.member)))
                } else {
                    later && (c.is_static || !decl_static || native_fields)
                }
            }
            Node::Param { .. } => {
                is_this
                    && !is_write
                    && native_fields
                    && !c.is_static
                    && self.is_parameter_property(d)
                    && self.param_property_class(d) == Some(c.class)
            }
            // Enum members, object literal properties and namespace exports declared after
            // a static initializer or static block that reads them.
            Node::EnumMember { .. }
            | Node::Property { .. }
            | Node::ShorthandProp { .. }
            | Node::VarDeclarator { .. }
            | Node::FunctionDecl { .. }
            | Node::Module { .. }
            | Node::Ident { .. } => {
                let var_name = matches!(ast.node(d.node), Node::Ident { .. });
                if !is_this
                    && c.is_static
                    && ast.span(d.node).start >= ast.span(e.node).start
                    && (!var_name
                        || matches!(
                            parents.parent(d.node).map(|x| ast.node(x)),
                            Some(Node::VarDeclarator { .. })
                        ))
                {
                    self.error(
                        pl,
                        dg::PROPERTY_0_IS_USED_BEFORE_ITS_INITIALIZATION_2729,
                        &[name],
                    );
                }
                return;
            }
            _ => false,
        };
        if report
            && (native_fields || !self.redeclares_base_member(Loc::new(e.file, c.class), name))
        {
            self.error(
                pl,
                dg::PROPERTY_0_IS_USED_BEFORE_ITS_INITIALIZATION_2729,
                &[name],
            );
        }
    }

    /// Whether the class `class` has a base whose instance type has a member `name`.
    fn redeclares_base_member(&mut self, class: Loc, name: &str) -> bool {
        let Some(sym) = self.c.sym_of_decl(class) else {
            return false;
        };
        let bases = self.base_types(sym);
        let Some(&base) = bases.first() else {
            return false;
        };
        base == ts_r_types::TypeId::UNRESOLVED
            || self.store().is_error(base)
            || self.property_of_type(base, name).is_some()
    }

    /// Whether a static block comes before `member` in the class body.
    fn static_block_before(&self, file: FileId, class: NodeId, member: NodeId) -> bool {
        let ast = self.ast(file);
        let members = match *ast.node(class) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return false,
        };
        let at = ast.span(member).start;
        ast.list(members)
            .iter()
            .any(|&m| matches!(ast.node(m), Node::StaticBlock { .. }) && ast.span(m).start < at)
    }

    /// The class whose constructor declares the parameter `d`.
    fn param_property_class(&self, d: Loc) -> Option<NodeId> {
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let func = parents.parent(d.node)?;
        let method = parents.parent(func)?;
        match *ast.node(method) {
            Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            } => parents.parent(method),
            _ => None,
        }
    }

    /// The property initializer or static block that evaluates `e` right away, if any.
    fn initializer_container(&self, e: Loc) -> Option<Container> {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut prev = e.node;
        let mut cur = parents.parent(e.node);
        let mut through_method_name = false;
        for _ in 0..4096 {
            let n = cur?;
            match *ast.node(n) {
                // The computed name of an object literal method or accessor is evaluated
                // with the literal.
                Node::MethodDef { key, .. }
                    if key == prev
                        && matches!(
                            parents.parent(n).map(|x| ast.node(x)),
                            Some(Node::ObjectLit { .. })
                        ) =>
                {
                    through_method_name = true;
                }
                // A field's decorators run when the class is defined.
                Node::Decorator { .. } => {
                    let member = parents.parent(n)?;
                    if !matches!(ast.node(member), Node::PropertyDef { .. }) {
                        return None;
                    }
                    let class = parents.parent(member)?;
                    return Some(Container {
                        member,
                        class,
                        is_static: true,
                        through_method_name,
                    });
                }
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::ClassDecl { .. } => return None,
                Node::PropertyDef {
                    value, modifiers, ..
                } if value == Some(prev) => {
                    let class = parents.parent(n)?;
                    return Some(Container {
                        member: n,
                        class,
                        is_static: modifiers.contains(Modifiers::STATIC),
                        through_method_name,
                    });
                }
                Node::StaticBlock { .. } => {
                    let class = parents.parent(n)?;
                    return Some(Container {
                        member: n,
                        class,
                        is_static: true,
                        through_method_name,
                    });
                }
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        None
    }
}
