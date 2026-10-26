//! From a byte offset to a node, and from a name to the symbols it means.

use ts_r_ast::{Ast, HeritageKind, Node, NodeId};
use ts_r_binder::{decl_name_node, meaning};
use ts_r_checker::{Ck, Loc};
use ts_r_types::{SymId, TypeId};

/// The deepest node whose span contains `offset`. A child that ends exactly at `offset` is
/// taken when no child contains it (a cursor right after a name points at that name).
pub fn node_at(ast: &Ast, offset: u32) -> Option<NodeId> {
    let root = ast.root;
    let sp = ast.span(root);
    if offset < sp.start || offset > sp.end {
        return None;
    }
    let mut cur = root;
    // Child ids are smaller than their parent's, so this descends at most node_count times.
    for _ in 0..ast.node_count() {
        let mut inside = None;
        let mut touching = None;
        ast.for_each_child(cur, |c| {
            let s = ast.span(c);
            if s.start == s.end {
                return;
            }
            if s.start <= offset && offset < s.end {
                if inside.is_none() {
                    inside = Some(c);
                }
            } else if s.end == offset {
                touching = Some(c);
            }
        });
        match inside.or(touching) {
            Some(c) if c != cur => cur = c,
            _ => break,
        }
    }
    Some(cur)
}

/// The identifier (or private name) at `offset`.
pub fn ident_at(ast: &Ast, offset: u32) -> Option<NodeId> {
    let n = node_at(ast, offset)?;
    is_name(ast, n).then_some(n)
}

pub fn is_name(ast: &Ast, n: NodeId) -> bool {
    matches!(ast.node(n), Node::Ident { .. } | Node::PrivateName { .. })
}

/// Text of an identifier or private name.
pub fn name_text(ast: &Ast, n: NodeId) -> Option<&str> {
    match *ast.node(n) {
        Node::Ident { name } | Node::PrivateName { name } => Some(ast.atom(name)),
        _ => None,
    }
}

/// What a name at a location refers to. Most names mean one symbol; a shorthand property
/// (`{ x }`, `const { x } = o`) means a property and a variable at once.
#[derive(Clone, Copy, Debug, Default)]
pub struct Meaning {
    pub primary: Option<SymId>,
    /// The other meaning of a shorthand: the variable for `{ x }` (primary is the property),
    /// the property for `const { x } = o` (primary is the variable).
    pub shorthand: Option<SymId>,
    /// The name declares `primary` (it is not a reference).
    pub is_decl: bool,
}

impl Meaning {
    pub fn iter(&self) -> impl Iterator<Item = SymId> {
        self.primary.into_iter().chain(self.shorthand)
    }
}

/// The symbols the name at `l` means (no alias resolution).
pub fn meaning_at(ck: &mut Ck<'_, '_>, l: Loc) -> Meaning {
    let c = ck.c;
    let ast = c.ast(l.file);
    let Some(name) = name_text(ast, l.node) else {
        return Meaning::default();
    };
    let parent = c.file(l.file).parents().parent(l.node);
    // A binding identifier declares its symbol directly.
    if let Some(s) = c.sym_of_decl(l) {
        let mut m = Meaning {
            primary: Some(s),
            shorthand: None,
            is_decl: true,
        };
        if let Some(p) = parent
            && let Node::BindingProperty {
                key, value: None, ..
            } = *ast.node(p)
            && key == l.node
        {
            m.shorthand = binding_property(ck, Loc::new(l.file, p), name);
        }
        return m;
    }
    let Some(p) = parent else {
        return Meaning::default();
    };
    let pl = Loc::new(l.file, p);
    let one = |s: Option<SymId>| Meaning {
        primary: s,
        shorthand: None,
        is_decl: false,
    };
    match *ast.node(p) {
        Node::Member { obj, prop, .. } if prop == l.node => {
            let t = ck.check_expr(Loc::new(l.file, obj));
            return one(accessed_property(ck, t, name));
        }
        Node::QualifiedName { left, right } if right == l.node => {
            let Some(ls) = entity_symbol(ck, Loc::new(l.file, left)) else {
                return Meaning::default();
            };
            let ls = c.resolve_alias(ls).unwrap_or(ls);
            return one(c.export_of(ls, name));
        }
        Node::ImportSpecifier {
            imported: Some(i), ..
        } if i == l.node => {
            return one(c.sym_of_decl(pl).and_then(|s| c.resolve_alias(s)));
        }
        Node::ExportSpecifier {
            local,
            exported: Some(_),
            ..
        } if local == l.node => {
            // `export { a as b }`: `a` is what `b` stands for.
            return one(c.sym_of_decl(pl).and_then(|s| c.resolve_alias(s)));
        }
        Node::ShorthandProp { name: n, .. } if n == l.node => {
            return Meaning {
                primary: c.sym_of_decl(pl),
                shorthand: c.resolve_ident(l, meaning::VALUE),
                is_decl: true,
            };
        }
        Node::BindingProperty { key, .. } if key == l.node => {
            return one(binding_property(ck, pl, name));
        }
        Node::JsxAttr { name: n, .. } if n == l.node => {
            return one(jsx_attribute(ck, pl, name));
        }
        _ => {}
    }
    if decl_name_node(ast, p) == Some(l.node)
        && let Some(s) = c.sym_of_decl(pl)
    {
        return Meaning {
            primary: Some(s),
            shorthand: None,
            is_decl: true,
        };
    }
    let want = match *ast.node(p) {
        Node::TypeRef { name: n, .. } if n == l.node => meaning::TYPE,
        Node::QualifiedName { left, .. } if left == l.node => meaning::NAMESPACE,
        Node::ExprWithTypeArgs { expr, .. } if expr == l.node => heritage_meaning(ck, pl),
        _ => meaning::VALUE,
    };
    one(c
        .resolve_ident(l, want)
        .or_else(|| c.resolve_ident(l, meaning::ALL)))
}

/// The symbol of an entity name (`A` or `A.B`), for qualified names.
pub fn entity_symbol(ck: &mut Ck<'_, '_>, l: Loc) -> Option<SymId> {
    let ast = ck.c.ast(l.file);
    match *ast.node(l.node) {
        Node::QualifiedName { right, .. } => meaning_at(ck, Loc::new(l.file, right)).primary,
        Node::Ident { .. } => {
            let c = ck.c;
            c.resolve_ident(l, meaning::NAMESPACE)
                .or_else(|| c.resolve_ident(l, meaning::ALL))
        }
        _ => None,
    }
}

/// `class C extends B` names a value; `implements I` and interface `extends I` name types.
fn heritage_meaning(ck: &Ck<'_, '_>, ewta: Loc) -> u8 {
    let c = ck.c;
    let Some(h) = c.parent(ewta) else {
        return meaning::ALL;
    };
    match *c.node(h) {
        Node::HeritageClause {
            kind: HeritageKind::Extends,
            ..
        } => {
            let class = c.parent(h).is_some_and(|p| {
                matches!(c.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. })
            });
            if class { meaning::VALUE } else { meaning::TYPE }
        }
        Node::HeritageClause { .. } => meaning::TYPE,
        _ => meaning::TYPE,
    }
}

/// A property of a type by name (through the apparent type: `"x".length`, unions).
pub fn property_of(ck: &mut Ck<'_, '_>, t: TypeId, name: &str) -> Option<SymId> {
    let a = ck.apparent_type(t);
    ck.property_of_type(a, name)
}

/// The property an access `o.name` reads: through `o`'s non-nullable part when the object
/// may be `undefined` (`a?.b`, an optional member). Destructuring does not look through
/// `undefined` (tsc finds no property there).
pub fn accessed_property(ck: &mut Ck<'_, '_>, t: TypeId, name: &str) -> Option<SymId> {
    if let Some(p) = property_of(ck, t, name) {
        return Some(p);
    }
    let nn = ck.non_nullable(t);
    if nn == t {
        return None;
    }
    property_of(ck, nn, name)
}

/// The property a destructuring key reads: `const { a } = o`, `function f({ a }: T)`.
fn binding_property(ck: &mut Ck<'_, '_>, bp: Loc, name: &str) -> Option<SymId> {
    let c = ck.c;
    let pattern = c.parent(bp)?;
    let t = pattern_source_type(ck, pattern)?;
    property_of(ck, t, name)
}

/// The type a binding pattern destructures (one level: declarators and parameters).
fn pattern_source_type(ck: &mut Ck<'_, '_>, pattern: Loc) -> Option<TypeId> {
    let c = ck.c;
    let holder = c.parent(pattern)?;
    match *c.node(holder) {
        Node::VarDeclarator {
            name,
            type_ann,
            init,
            ..
        } if name == pattern.node => {
            if let Some(t) = type_ann {
                return Some(ck.type_from_node(Loc::new(pattern.file, t)));
            }
            init.map(|i| ck.check_expr(Loc::new(pattern.file, i)))
        }
        Node::Param { pattern: p, .. } if p == pattern.node => Some(ck.param_type(holder)),
        Node::BindingProperty { value: Some(v), .. } if v == pattern.node => {
            // Nested: `{ a: { b } }`.
            let outer = c.parent(holder)?;
            let key = match *c.node(holder) {
                Node::BindingProperty { key, .. } => key,
                _ => return None,
            };
            let name = name_text(c.ast(pattern.file), key)?.to_string();
            let t = pattern_source_type(ck, outer)?;
            let p = property_of(ck, t, &name)?;
            Some(ck.type_of_symbol(p))
        }
        _ => None,
    }
}

/// The props property a JSX attribute sets.
fn jsx_attribute(ck: &mut Ck<'_, '_>, attr: Loc, name: &str) -> Option<SymId> {
    let c = ck.c;
    let opening = c.parent(attr)?;
    let Node::JsxOpening { name: tag, .. } = *c.node(opening) else {
        return None;
    };
    // Intrinsic elements (`div`) have no symbol here; components do.
    let tag_l = Loc::new(attr.file, tag);
    if !matches!(
        c.node(tag_l),
        Node::Ident { .. } | Node::JsxMemberExpr { .. }
    ) {
        return None;
    }
    if let Node::Ident { name: a } = *c.node(tag_l)
        && c.ast(attr.file)
            .atom(a)
            .starts_with(|ch: char| ch.is_ascii_lowercase())
    {
        return None;
    }
    let t = ck.check_expr(tag_l);
    let sigs = ck.signatures_of(t, ts_r_checker::members::SigKind::Call);
    let sig = *sigs.first()?;
    let props = ck.store().sig(sig).params.first()?.ty;
    property_of(ck, props, name)
}

/// Whether the name at `l` is evaluated as an expression (so `check_expr` gives its
/// narrowed type).
pub fn is_expression_name(ck: &Ck<'_, '_>, l: Loc) -> bool {
    let c = ck.c;
    let Some(p) = c.parent(l) else {
        return false;
    };
    let n = l.node;
    match *c.node(p) {
        Node::VarDeclarator { init, .. } => init == Some(n),
        Node::Param { init, .. } => init == Some(n),
        Node::Property { value, .. } => value == n,
        Node::PropertyDef { value, .. } => value == Some(n),
        Node::Member { obj, .. } => obj == n,
        Node::Index { .. }
        | Node::ExprStmt { .. }
        | Node::If { .. }
        | Node::For { .. }
        | Node::ForIn { .. }
        | Node::ForOf { .. }
        | Node::While { .. }
        | Node::DoWhile { .. }
        | Node::Return { .. }
        | Node::Throw { .. }
        | Node::Switch { .. }
        | Node::SwitchCase { .. }
        | Node::Spread { .. }
        | Node::Paren { .. }
        | Node::Seq { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Await { .. }
        | Node::Yield { .. }
        | Node::Binary { .. }
        | Node::Assign { .. }
        | Node::Conditional { .. }
        | Node::Call { .. }
        | Node::New { .. }
        | Node::As { .. }
        | Node::Satisfies { .. }
        | Node::TypeAssertion { .. }
        | Node::NonNull { .. }
        | Node::ArrayLit { .. }
        | Node::TemplateLit { .. }
        | Node::TaggedTemplate { .. }
        | Node::JsxExprContainer { .. }
        | Node::JsxSpreadAttr { .. }
        | Node::ExportDefaultExpr { .. }
        | Node::Decorator { .. }
        | Node::ComputedKey { .. } => true,
        Node::Arrow { body, .. } => body == n,
        Node::AssignPattern { right, .. } => right == n,
        _ => false,
    }
}

/// Every element access with a string-literal key (`o["x"]`) in a file, in source order:
/// `(access, literal)`.
pub fn literal_accesses_in(ast: &Ast) -> Vec<(NodeId, NodeId)> {
    struct V(Vec<(NodeId, NodeId)>);
    impl ts_r_ast::visit::Visitor for V {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            if let Node::Index { index, .. } = *ast.node(id)
                && matches!(ast.node(index), Node::StringLit { .. })
            {
                self.0.push((id, index));
            }
            ts_r_ast::visit::Walk::Continue
        }
    }
    let mut v = V(Vec::new());
    ts_r_ast::visit::walk(ast, ast.root, &mut v);
    v.0
}

/// Every name node in a file, in source order.
pub fn names_in(ast: &Ast) -> Vec<NodeId> {
    struct V(Vec<NodeId>);
    impl ts_r_ast::visit::Visitor for V {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            if is_name(ast, id) {
                self.0.push(id);
            }
            ts_r_ast::visit::Walk::Continue
        }
    }
    let mut v = V(Vec::new());
    ts_r_ast::visit::walk(ast, ast.root, &mut v);
    v.0
}
