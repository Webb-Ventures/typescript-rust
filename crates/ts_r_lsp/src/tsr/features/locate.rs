//! From a byte offset to a node, and from a name to the symbols it means.

use ts_r_ast::{AssignOp, Ast, HeritageKind, Node, NodeId};
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
/// The name at `offset`, or the one ending there (`a|()` is on `a`, as in tsserver).
pub fn ident_at(ast: &Ast, offset: u32) -> Option<NodeId> {
    let n = node_at(ast, offset)?;
    if is_name(ast, n) {
        return Some(n);
    }
    let p = node_at(ast, offset.checked_sub(1)?)?;
    (is_name(ast, p) && ast.span(p).end == offset).then_some(p)
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
    /// For a property of a contextually typed object literal (`const a: I = { x: 1 }`),
    /// the property of the contextual type it implements; references and rename treat the
    /// two as one.
    pub contextual: Option<SymId>,
}

impl Meaning {
    pub fn iter(&self) -> impl Iterator<Item = SymId> {
        self.primary.into_iter().chain(self.shorthand)
    }
}

/// The symbols the name at `l` means (no alias resolution).
pub fn meaning_at(ck: &mut Ck<'_, '_>, l: Loc) -> Meaning {
    let mut m = if is_name(ck.c.ast(l.file), l.node) {
        own_meaning(ck, l)
    } else {
        literal_key_meaning(ck, l)
    };
    m.contextual = contextual_property(ck, l);
    m
}

/// The property name a string or numeric literal stands for where it names a member
/// (`"a": 1`, `get 42()`, `{ "a": string }`) or indexes one (`o["a"]`, `o[0]`).
pub fn literal_key(ast: &Ast, parents: &ts_r_ast::ParentMap, n: NodeId) -> Option<String> {
    let text = match *ast.node(n) {
        Node::StringLit { value } => ast.atom(value).to_string(),
        Node::NumberLit { value } => ts_r_binder::js_number_to_string(value.get()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement { cooked, .. } => ast.atom(cooked).to_string(),
                _ => return None,
            },
            _ => return None,
        },
        _ => return None,
    };
    let mut key = n;
    let mut p = parents.parent(n)?;
    // `["a"]: 1`, `` [`a`]() {} ``: a computed key spelled as a literal.
    if matches!(*ast.node(p), Node::ComputedKey { expr } if expr == n) {
        key = p;
        p = parents.parent(p)?;
    }
    let keyed = match *ast.node(p) {
        Node::PropertyDef { key: k, .. }
        | Node::MethodDef { key: k, .. }
        | Node::PropertySignature { key: k, .. }
        | Node::MethodSignature { key: k, .. }
        | Node::Property { key: k, .. } => k == key,
        Node::Index { index, .. } => index == n,
        Node::EnumMember { name, .. } => name == n,
        _ => false,
    };
    keyed.then_some(text)
}

/// The member a literal key belongs to (through a computed key), for [`literal_key`] nodes.
pub fn literal_key_owner(ck: &Ck<'_, '_>, l: Loc) -> Option<Loc> {
    let c = ck.c;
    let p = c.parent(l)?;
    if matches!(*c.node(p), Node::ComputedKey { .. }) {
        return c.parent(p);
    }
    Some(p)
}

/// The text a name or a literal key ([`literal_key`]) spells.
pub fn key_text(ck: &Ck<'_, '_>, l: Loc) -> Option<String> {
    let ast = ck.c.ast(l.file);
    match name_text(ast, l.node) {
        Some(t) => Some(t.to_string()),
        None => literal_key(ast, ck.c.file(l.file).parents(), l.node),
    }
}

/// The span a reference at a literal key covers: the text between the quotes when it is
/// spelled without escapes, the whole number for a numeric key.
pub fn literal_key_span(ast: &Ast, text: &str, n: NodeId, key: &str) -> Option<(u32, u32)> {
    let sp = ast.span(n);
    match ast.node(n) {
        Node::StringLit { .. } | Node::TemplateLit { .. } => {
            let (start, end) = (sp.start + 1, sp.end.saturating_sub(1));
            (text.get(start as usize..end as usize) == Some(key)).then_some((start, end))
        }
        _ => Some((sp.start, sp.end)),
    }
}

/// Every string or numeric literal in a file that names or indexes a member, in source order.
pub fn literal_keys_in(ast: &Ast, parents: &ts_r_ast::ParentMap) -> Vec<NodeId> {
    struct V<'p>(Vec<NodeId>, &'p ts_r_ast::ParentMap);
    impl ts_r_ast::visit::Visitor for V<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            if matches!(
                ast.node(id),
                Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. }
            ) && literal_key(ast, self.1, id).is_some()
            {
                self.0.push(id);
            }
            ts_r_ast::visit::Walk::Continue
        }
    }
    let mut v = V(Vec::new(), parents);
    ts_r_ast::visit::walk(ast, ast.root, &mut v);
    v.0
}

/// [`meaning_at`] for a literal key: the member it declares, or the property it indexes.
fn literal_key_meaning(ck: &mut Ck<'_, '_>, l: Loc) -> Meaning {
    let c = ck.c;
    let Some(key) = key_text(ck, l) else {
        return Meaning::default();
    };
    let Some(p) = literal_key_owner(ck, l) else {
        return Meaning::default();
    };
    if let Node::Index { obj, .. } = *c.node(p) {
        let t = ck.check_expr(Loc::new(l.file, obj));
        return Meaning {
            primary: accessed_property(ck, t, &key),
            ..Meaning::default()
        };
    }
    Meaning {
        primary: c.sym_of_decl(p),
        is_decl: true,
        ..Meaning::default()
    }
}

/// The property of the contextual type that an object literal member named at `l`
/// implements, when the literal is not a destructuring target.
fn contextual_property(ck: &mut Ck<'_, '_>, l: Loc) -> Option<SymId> {
    let c = ck.c;
    let mut key = l.node;
    let mut member = c.parent(l)?;
    if matches!(*c.node(member), Node::ComputedKey { .. }) && !is_name(c.ast(l.file), l.node) {
        key = member.node;
        member = c.parent(member)?;
    }
    let named = match *c.node(member) {
        Node::Property { key: k, .. } => k == key,
        Node::ShorthandProp { name, .. } => name == key,
        Node::MethodDef { key: k, .. } => k == key,
        _ => false,
    };
    let lit = c.parent(member)?;
    if !named || !matches!(c.node(lit), Node::ObjectLit { .. }) {
        return None;
    }
    if literal_target_source(ck, lit, 0).is_some() {
        return None;
    }
    let name = key_text(ck, l)?;
    let t = ck.contextual_type(lit)?;
    property_of(ck, t, &name)
}

/// [`meaning_at`] without the contextual property.
fn own_meaning(ck: &mut Ck<'_, '_>, l: Loc) -> Meaning {
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
            contextual: None,
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
        contextual: None,
    };
    // `this.#x`, `#x in o`: the member of the innermost class declaring `#x`.
    if matches!(ast.node(l.node), Node::PrivateName { .. })
        && decl_name_node(ast, p) != Some(l.node)
    {
        return one(private_member(ck, l, name));
    }
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
        Node::Property { key, .. } if key == l.node => {
            // `({ a: x } = o)`: the key reads `o.a`.
            if let Some(t) = target_source(ck, pl) {
                return one(property_of(ck, t, name));
            }
        }
        Node::ShorthandProp { name: n, .. } if n == l.node => {
            // `({ a } = o)` reads `o.a` into the variable `a`.
            let primary = match target_source(ck, pl) {
                Some(t) => property_of(ck, t, name),
                None => c.sym_of_decl(pl),
            };
            return Meaning {
                primary,
                shorthand: c.resolve_ident(l, meaning::VALUE),
                is_decl: true,
                contextual: None,
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
            contextual: None,
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

/// The member `#name` of the innermost class around `l` that declares it (private names
/// are scoped lexically, not by the object's type).
fn private_member(ck: &Ck<'_, '_>, l: Loc, name: &str) -> Option<SymId> {
    let c = ck.c;
    let ast = c.ast(l.file);
    let mut cur = l;
    for _ in 0..256 {
        cur = c.parent(cur)?;
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) =
            *ast.node(cur.node)
        else {
            continue;
        };
        for &m in ast.list(members) {
            let key = match *ast.node(m) {
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                _ => continue,
            };
            if matches!(ast.node(key), Node::PrivateName { .. })
                && name_text(ast, key) == Some(name)
            {
                return c.sym_of_decl(Loc::new(l.file, m));
            }
        }
    }
    None
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
pub(super) fn binding_property(ck: &mut Ck<'_, '_>, bp: Loc, name: &str) -> Option<SymId> {
    let c = ck.c;
    let pattern = c.parent(bp)?;
    let t = pattern_source_type(ck, pattern)?;
    property_of(ck, t, name)
}

/// The type a binding pattern destructures (one level: declarators and parameters).
pub(super) fn pattern_source_type(ck: &mut Ck<'_, '_>, pattern: Loc) -> Option<TypeId> {
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
            if let Some(i) = init {
                return Some(ck.check_expr(Loc::new(pattern.file, i)));
            }
            // `for (const { a } of xs)`: each element of `xs`.
            let list = c.parent(holder)?;
            let stmt = c.parent(list)?;
            match *c.node(stmt) {
                Node::ForOf {
                    is_await,
                    left,
                    right,
                    ..
                } if left == list.node => {
                    let t = ck.check_expr(Loc::new(pattern.file, right));
                    ck.iterated_type(t, is_await)
                }
                _ => None,
            }
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

/// The type the object literal holding the property `prop` takes apart when that literal is
/// a destructuring assignment target: `({ a } = o)`, `for ({ a } of xs)`, `[{ a }] = xs`
/// and nested targets.
fn target_source(ck: &mut Ck<'_, '_>, prop: Loc) -> Option<TypeId> {
    let lit = ck.c.parent(prop)?;
    if !matches!(ck.c.node(lit), Node::ObjectLit { .. }) {
        return None;
    }
    literal_target_source(ck, lit, 0)
}

/// [`target_source`] for an object or array literal `lit`.
fn literal_target_source(ck: &mut Ck<'_, '_>, lit: Loc, depth: u32) -> Option<TypeId> {
    const MAX_DEPTH: u32 = 32;
    if depth > MAX_DEPTH {
        return None;
    }
    let c = ck.c;
    let holder = c.parent(lit)?;
    match *c.node(holder) {
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } if left == lit.node => Some(ck.check_expr(Loc::new(lit.file, right))),
        Node::ForOf {
            is_await,
            left,
            right,
            ..
        } if left == lit.node => {
            let t = ck.check_expr(Loc::new(lit.file, right));
            ck.iterated_type(t, is_await)
        }
        Node::Property { key, value, .. } if value == lit.node => {
            let outer = c.parent(holder)?;
            if !matches!(c.node(outer), Node::ObjectLit { .. }) {
                return None;
            }
            let name = name_text(c.ast(lit.file), key)?.to_string();
            let t = literal_target_source(ck, outer, depth + 1)?;
            let p = property_of(ck, t, &name)?;
            Some(ck.type_of_symbol(p))
        }
        Node::ArrayLit { elements } => {
            let i = c
                .ast(lit.file)
                .list(elements)
                .iter()
                .position(|&e| e == lit.node)?;
            let t = literal_target_source(ck, holder, depth + 1)?;
            match property_of(ck, t, &i.to_string()) {
                Some(p) => Some(ck.type_of_symbol(p)),
                None => ck.element_type_of_array_like(t),
            }
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
