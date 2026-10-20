use crate::*;

#[test]
fn type_data_is_small() {
    assert!(
        std::mem::size_of::<TypeData>() <= 24,
        "{}",
        std::mem::size_of::<TypeData>()
    );
    assert!(std::mem::size_of::<TypeKind>() <= 12);
}

#[test]
fn intrinsics_have_fixed_ids() {
    let s = TypeStore::new();
    assert_eq!(s.len() as u32, TypeId::INTRINSIC_COUNT);
    assert!(s.flags(TypeId::STRING).contains(TypeFlags::STRING));
    assert!(
        s.flags(TypeId::BOOLEAN)
            .contains(TypeFlags::UNION | TypeFlags::BOOLEAN)
    );
    assert_eq!(
        s.members_of_union(TypeId::BOOLEAN),
        &[TypeId::FALSE, TypeId::TRUE]
    );
    assert_eq!(s.regular(TypeId::FRESH_TRUE), TypeId::TRUE);
    assert_eq!(s.fresh(TypeId::FALSE), TypeId::FRESH_FALSE);
    assert!(
        s.members(TypeId::EMPTY_OBJECT)
            .is_some_and(|m| m.is_empty())
    );
}

#[test]
fn structural_interning() {
    let s = TypeStore::new();
    let a = s.string_literal("a", false);
    let fa = s.string_literal("a", true);
    assert_ne!(a, fa);
    assert_eq!(s.regular(fa), a);
    assert_eq!(s.fresh(a), fa);
    assert_eq!(s.number_literal(0.0, false), s.number_literal(-0.0, false));
    assert_eq!(s.number_value(s.number_literal(1.5, false)), Some(1.5));
    let u1 = s.union_sorted(&[a, TypeId::NUMBER], None);
    let u2 = s.union_sorted(&[a, TypeId::NUMBER], None);
    assert_eq!(u1, u2);
    let alias = s.alias(SymId(7), ListId::EMPTY);
    let u3 = s.union_sorted(&[a, TypeId::NUMBER], Some(alias));
    assert_ne!(u1, u3, "aliased unions are distinct types");
    assert_eq!(s.alias_of(u3).map(|a| a.sym), Some(SymId(7)));
    let l = s.list(&[a, TypeId::NUMBER]);
    assert_eq!(s.list(&[a, TypeId::NUMBER]), l);
    assert_eq!(s.list_items(l), &[a, TypeId::NUMBER]);
}

#[test]
fn references_and_tuples() {
    let s = TypeStore::new();
    let p = s.type_param(SymId(1), false);
    let decl = s.declared_object(SymId(2), ObjectFlags::INTERFACE, || {
        (s.list(&[p]), s.type_param(SymId(2), true))
    });
    assert_eq!(
        decl,
        s.declared_object(SymId(2), ObjectFlags::INTERFACE, || unreachable!())
    );
    let r1 = s.reference(decl, &[TypeId::STRING]);
    assert_eq!(r1, s.reference(decl, &[TypeId::STRING]));
    assert_ne!(r1, s.reference(decl, &[TypeId::NUMBER]));
    let shape = store_tuple(&s);
    let t = s.reference(shape, &[TypeId::NUMBER, TypeId::STRING]);
    let sh = s
        .tuple_shape_of(t)
        .map(|x| (x.min_length(), x.fixed_length(), x.has_rest()));
    assert_eq!(sh, Some((1, 2, false)));
}

fn store_tuple(s: &TypeStore) -> TypeId {
    use crate::store::{TupleElement, TupleShape};
    s.tuple_target(TupleShape {
        elements: vec![
            TupleElement {
                kind: TupleElement::REQUIRED,
                label: None,
            },
            TupleElement {
                kind: TupleElement::OPTIONAL,
                label: None,
            },
        ]
        .into_boxed_slice(),
        readonly: false,
    })
}

#[test]
fn members_are_write_once() {
    let s = TypeStore::new();
    let t = s.anonymous(
        (0, 3),
        0,
        MapperId::IDENTITY,
        ObjectFlags::ANONYMOUS,
        AliasId::NONE,
        false,
    );
    assert!(s.members(t).is_none());
    let name = s.intern_str("x");
    let m = s.set_members(
        t,
        ResolvedMembers::new(vec![(name, SymId(9))], vec![], vec![], vec![]),
    );
    assert_eq!(m.find(name), Some(SymId(9)));
    let m2 = s.set_members(t, ResolvedMembers::empty());
    assert_eq!(m2.find(name), Some(SymId(9)), "first writer wins");
}

#[test]
fn parallel_interning_gives_one_id() {
    let s = TypeStore::new();
    let ids: Vec<Vec<TypeId>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4)
            .map(|_| {
                sc.spawn(|| {
                    (0..2000)
                        .map(|i| {
                            let a = s.string_literal(&format!("s{i}"), false);
                            s.union_sorted(&[a, TypeId::NUMBER], None)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    for v in &ids[1..] {
        assert_eq!(v, &ids[0]);
    }
}

#[test]
fn relation_cache() {
    let s = TypeStore::new();
    let c = s.relation_cache(RelationKind::Assignable);
    assert_eq!(c.get(TypeId::STRING, TypeId::NUMBER), None);
    c.set(TypeId::STRING, TypeId::NUMBER, false);
    assert_eq!(c.get(TypeId::STRING, TypeId::NUMBER), Some(false));
}

#[test]
fn mappers() {
    let s = TypeStore::new();
    let p = s.type_param(SymId(1), false);
    let m = s.mapper(&[p], &[TypeId::STRING]);
    assert_eq!(m, s.mapper(&[p], &[TypeId::STRING]));
    assert_eq!(s.map_type(m, p), Some(TypeId::STRING));
    assert_eq!(s.map_type(m, TypeId::NUMBER), None);
    assert_eq!(s.mapper(&[], &[]), MapperId::IDENTITY);
    assert!(MapperId::PERMISSIVE.is_special() && MapperId::RESTRICTIVE.is_special());
    assert!(!m.is_special());
}

#[test]
fn mapped_conditional_and_template_types() {
    let s = TypeStore::new();
    let p = s.type_param(SymId(1), false);
    // Mapped types are keyed by declaration, mapper and alias.
    let m1 = s.mapped_object((0, 5), MapperId::IDENTITY, AliasId::NONE, true);
    assert_eq!(
        m1,
        s.mapped_object((0, 5), MapperId::IDENTITY, AliasId::NONE, true)
    );
    let inst = s.mapper(&[p], &[TypeId::STRING]);
    let m2 = s.mapped_object((0, 5), inst, AliasId::NONE, false);
    assert_ne!(m1, m2);
    assert!(s.traits(m1).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
    assert!(!s.traits(m2).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
    // One root per conditional node; deferred conditionals by root and mapper.
    let root = s.conditional_root((0, 9), || ConditionalRoot {
        decl: (0, 9),
        check: p,
        extends: TypeId::STRING,
        infer_params: ListId::EMPTY,
        outer_params: s.list(&[p]),
        distributive: true,
    });
    assert_eq!(root, s.conditional_root((0, 9), || unreachable!()));
    let c = s.conditional(root, MapperId::IDENTITY, AliasId::NONE);
    assert_eq!(c, s.conditional(root, MapperId::IDENTITY, AliasId::NONE));
    assert!(s.flags(c).contains(TypeFlags::CONDITIONAL));
    assert_eq!(s.conditional_data(c).map(|d| d.root), Some(root));
    // Template literal types intern by texts and spans.
    let a = s.string_literal("a", false);
    let b = s.string_literal("", false);
    let t = s.template_literal(&[a, b], &[p]);
    assert_eq!(t, s.template_literal(&[a, b], &[p]));
    assert!(s.flags(t).contains(TypeFlags::TEMPLATE_LITERAL));
    assert!(s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
    assert_eq!(s.template_parts(t), Some((&[a, b][..], &[p][..])));
    // The restrictive copy of a type parameter is unique and is its own restrictive copy.
    let r = s.restrictive_type_param(p);
    assert_ne!(r, p);
    assert_eq!(r, s.restrictive_type_param(p));
    assert_eq!(s.restrictive_type_param(r), r);
}
