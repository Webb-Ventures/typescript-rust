//! Declaration checks run once per file after the statement walk (track M5 "checker
//! decls"): unused declarations, class and member rules, overload sets, duplicate
//! declarations and decorators. Each rule family lives in its own module; this is the
//! entry point `check.rs` calls.

use ts_r_ast::{Node, NodeId};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};

/// Node budget of the declaration walk (pathological files stop early).
const MAX_NODES: u32 = 5_000_000;

impl<'c, 'a> Ck<'c, 'a> {
    /// Runs the declaration checks of `file`.
    pub(crate) fn check_decls(&mut self, file: FileId) {
        let sf = self.c.file(file);
        let ast = &sf.ast;
        let mut classes: Vec<NodeId> = Vec::new();
        let mut objects: Vec<NodeId> = Vec::new();
        let mut interfaces: Vec<NodeId> = Vec::new();
        let mut literals: Vec<NodeId> = Vec::new();
        let mut vars: Vec<NodeId> = Vec::new();
        let mut props: Vec<NodeId> = Vec::new();
        let mut decorated: Vec<NodeId> = Vec::new();
        let mut lists: Vec<NodeId> = Vec::new();
        let mut sigs: Vec<NodeId> = Vec::new();
        let mut named: Vec<NodeId> = Vec::new();
        let mut merges: Vec<NodeId> = Vec::new();
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > MAX_NODES {
                break;
            }
            if matches!(
                ast.node(n),
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
                    | Node::StaticBlock { .. }
                    | Node::ClassExpr { .. }
                    | Node::MethodDef { .. }
                    | Node::PropertyDef { .. }
                    | Node::Param { .. }
            ) {
                decorated.push(n);
            }
            if matches!(
                ast.node(n),
                Node::FunctionDecl { .. }
                    | Node::MethodDef { .. }
                    | Node::MethodSignature { .. }
                    | Node::ConstructSignature { .. }
                    | Node::CallSignature { .. }
            ) {
                sigs.push(n);
            }
            if matches!(
                ast.node(n),
                Node::Program { .. }
                    | Node::Block { .. }
                    | Node::ModuleBlock { .. }
                    | Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
            ) {
                lists.push(n);
            }
            if matches!(
                ast.node(n),
                Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
                    | Node::Interface { .. }
                    | Node::TypeAlias { .. }
                    | Node::Enum { .. }
                    | Node::TypeParam { .. }
            ) {
                named.push(n);
            }
            if matches!(ast.node(n), Node::Module { .. } | Node::Enum { .. }) {
                merges.push(n);
            }
            match ast.node(n) {
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => classes.push(n),
                Node::ObjectLit { .. } => objects.push(n),
                Node::Interface { .. } => interfaces.push(n),
                Node::TypeLiteral { .. } => literals.push(n),
                Node::VarDeclarator { .. } => vars.push(n),
                Node::PropertySignature { .. } | Node::PropertyDef { .. } => props.push(n),
                // Parameter properties.
                Node::Param { modifiers, .. } if !modifiers.is_empty() => props.push(n),
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        classes.sort_unstable();
        objects.sort_unstable();
        interfaces.sort_unstable();
        literals.sort_unstable();
        vars.sort_unstable();
        decorated.sort_unstable();
        lists.sort_unstable();
        sigs.sort_unstable();
        named.sort_unstable();
        for n in named {
            self.check_declaration_name(file, n);
        }
        merges.sort_unstable();
        for n in merges {
            if matches!(self.ast(file).node(n), Node::Enum { .. }) {
                self.check_enum_merge_initializers(Loc::new(file, n));
            } else {
                self.check_namespace_merge_order(Loc::new(file, n));
                self.check_namespace_prototype_export(Loc::new(file, n));
            }
        }
        for n in sigs {
            self.check_implicit_any_return(file, n);
        }
        for l in lists {
            self.check_overloads(file, l);
        }
        for d in decorated {
            self.check_decorator_placement(file, d);
            self.check_decorator_expressions(file, d);
        }
        props.sort_unstable();
        for v in vars {
            self.check_var_redeclaration(file, v);
        }
        for p in props {
            self.check_property_redeclaration(file, p);
            self.check_identical_modifiers(file, p);
        }
        for c in classes {
            if matches!(self.ast(file).node(c), Node::ClassDecl { .. }) {
                self.check_decorator_placement(file, c);
                self.check_decorator_expressions(file, c);
            }
            self.check_type_parameter_merge(Loc::new(file, c));
            self.check_base_cycle(Loc::new(file, c));
            self.check_class_decl(Loc::new(file, c));
            self.check_class_abstracts(Loc::new(file, c));
            self.check_class_overrides(Loc::new(file, c));
            self.check_index_constraints(Loc::new(file, c));
            self.check_late_bound_duplicates(Loc::new(file, c));
            self.check_computed_member_names(Loc::new(file, c));
        }
        for i in interfaces {
            self.check_type_parameter_merge(Loc::new(file, i));
            self.check_interface_heritage(Loc::new(file, i));
            self.check_circular_interface_bases(Loc::new(file, i));
            self.check_interface_base_kinds(Loc::new(file, i));
            self.check_index_constraints(Loc::new(file, i));
            self.check_late_bound_duplicates(Loc::new(file, i));
            self.check_computed_member_names(Loc::new(file, i));
        }
        for t in literals {
            self.check_index_constraints(Loc::new(file, t));
            self.check_late_bound_duplicates(Loc::new(file, t));
            self.check_computed_member_names(Loc::new(file, t));
        }
        for o in objects {
            self.check_object_accessors(Loc::new(file, o));
        }
        self.check_import_conflicts(file);
        self.check_export_specifier_conflicts(file);
        self.check_isolated_module_aliases(file);
        self.check_verbatim_commonjs_namespaces(file);
        self.check_isolated_script_namespaces(file);
        self.check_unused(file);
    }
}
