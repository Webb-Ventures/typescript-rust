//! Declaration index: what changes the hashes and what does not, explicitness, deps, the
//! binary encoding and the cache round trip.

use ts_r_ast::SourceKind;
use ts_r_binder::{BindOptions, bind};
use ts_r_decls::{DeclIndex, Dep, Section, cache_key, get_or_compute, index};

fn idx_of(src: &str, path: &str) -> DeclIndex {
    let p = ts_r_parser::parse(src, SourceKind::from_path(path));
    let b = bind(&p.ast, src, &BindOptions::default());
    index(&p.ast, &b)
}

fn idx(src: &str) -> DeclIndex {
    idx_of(src, "a.ts")
}

fn idx_js(src: &str) -> DeclIndex {
    let mut p = ts_r_parser::parse(src, SourceKind::Js);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let diagnostics = ts_r_parser::attach_jsdoc(&mut p.ast, src);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let b = bind(&p.ast, src, &BindOptions::default());
    index(&p.ast, &b)
}

fn entry_hash(i: &DeclIndex, name: &str) -> [u8; 32] {
    i.entry(Section::Export, name).expect(name).hash
}

#[test]
fn bodies_whitespace_and_comments_do_not_change_explicit_signatures() {
    let a = idx(
        "export function f(x: number): string { return String(x); }\nexport const c: number = 1;\n",
    );
    let b = idx(
        "// comment\nexport   function f(x: number): string {\n  const y = x * 2;\n  return `${y}`;\n}\n\nexport const c: number = 2 + 3;\n",
    );
    assert!(a.is_explicit());
    assert_eq!(a.api_hash, b.api_hash);
    assert_eq!(a.entries, b.entries);
}

#[test]
fn signature_changes_change_the_hash() {
    let base = idx("export function f(x: number): string { return ''; }\n");
    for changed in [
        "export function f(x: string): string { return ''; }\n",
        "export function f(x: number): number { return 0; }\n",
        "export function f(x?: number): string { return ''; }\n",
        "export function f(x: number = 1): string { return ''; }\n",
        "export function f<T>(x: number): string { return ''; }\n",
        "export async function f(x: number): string { return ''; }\n",
        "export function g(x: number): string { return ''; }\n",
    ] {
        assert_ne!(base.api_hash, idx(changed).api_hash, "{changed}");
    }
}

#[test]
fn detached_jsdoc_types_change_the_signature() {
    for src in [
        "/** @type {() => number} */ export function f() { throw 0; }",
        "export const f = /** @type {() => number} */ () => { throw 0; };",
        "export const f = /** @type {() => number} */ function() { throw 0; };",
        "export const o = {\n/** @type {number} */ x: 0\n};",
        "export function f() {}\n/** @type {number} */ f.x = 0;",
        "/** @type {number} */ export default 0;",
        "/** @overload @param {number} a @returns {void} */\nexport function f(a) {}",
        "export class C {\n/** @overload @param {number} a @returns {void} */\nm(a) {}\n}",
    ] {
        let base = idx_js(src);
        let changed = idx_js(&src.replace("number", "string"));
        assert_ne!(base.api_hash, changed.api_hash, "{src}");
        let moved = idx_js(&format!("// ordinary comment\n\n{src}"));
        assert_eq!(base.api_hash, moved.api_hash, "{src}");
    }
}

#[test]
fn detached_jsdoc_types_record_dependencies() {
    let imported = idx_js(
        "import { T } from './dep';\n/** @type {() => T} */ export function f() { throw 0; }",
    );
    assert!(
        imported
            .entry(Section::Export, "f")
            .unwrap()
            .deps
            .contains(&Dep::Import {
                specifier: "./dep".into(),
                name: "T".into(),
            })
    );
    let global = idx_js("/** @type {() => Window} */ export function f() { throw 0; }");
    assert!(
        global
            .entry(Section::Export, "f")
            .unwrap()
            .deps
            .contains(&Dep::Global("Window".into()))
    );
    // A module's `@typedef` is exported, so `f` depends on it rather than hashing it.
    let local = "/** @typedef {{ x: number }} Local */\n/** @type {() => Local} */ export function f() { throw 0; }";
    let before = idx_js(local);
    assert!(
        before
            .entry(Section::Export, "f")
            .unwrap()
            .deps
            .contains(&Dep::Sibling("Local".into()))
    );
    assert_ne!(
        entry_hash(&before, "Local"),
        entry_hash(&idx_js(&local.replace("number", "string")), "Local")
    );
}

#[test]
fn expando_assignments_are_part_of_the_function() {
    let base = idx("export function f(): void {}\nf.x = 1;\n");
    assert!(!base.entry(Section::Export, "f").unwrap().explicit);
    for changed in [
        "export function f(): void {}\nf.x = \"s\";\n",
        "export function f(): void {}\nf.y = 1;\n",
        "export function f(): void {}\n",
        "export function f(): void {}\nconst k = \"x\";\nf[k] = 1;\n",
        "export const f = (): void => {};\nf.x = 1;\n",
    ] {
        assert_ne!(
            entry_hash(&base, "f"),
            entry_hash(&idx(changed), "f"),
            "{changed}"
        );
    }
    // A late-bound key's own declaration is a dependency of the entry.
    let a = idx("export function f(): void {}\nconst k = \"x\";\nf[k] = 1;\n");
    let b = idx("export function f(): void {}\nconst k = \"y\";\nf[k] = 1;\n");
    assert_ne!(entry_hash(&a, "f"), entry_hash(&b, "f"));
}

#[test]
fn inferred_types_hash_their_sources() {
    let a = idx("export function f(x: number) { return x; }\nexport const v = 1;\n");
    let b = idx("export function f(x: number) { return String(x); }\nexport const v = 1;\n");
    assert!(!a.entry(Section::Export, "f").unwrap().explicit);
    assert!(!a.entry(Section::Export, "v").unwrap().explicit);
    assert_ne!(entry_hash(&a, "f"), entry_hash(&b, "f"));
    assert_eq!(entry_hash(&a, "v"), entry_hash(&b, "v"));
}

#[test]
fn classes_skip_private_types_and_member_bodies() {
    let a = idx(
        "export class C {\n  private secret: number = 1;\n  #hidden = 2;\n  x: string = 'a';\n  constructor(public p: number) { this.x = ''; }\n  m(): void { this.x = 'b'; }\n  set s(v: number) { this.secret = v; }\n}\n",
    );
    let b = idx(
        "export class C {\n  private secret: string = 'z';\n  #hidden = 'q';\n  x: string = 'other';\n  constructor(public p: number) { }\n  m(): void { }\n  set s(v: number) { }\n}\n",
    );
    assert_eq!(a.api_hash, b.api_hash);
    let c = idx(
        "export class C {\n  private secret: number = 1;\n  #hidden = 2;\n  x: number = 1;\n  constructor(public p: number) {}\n  m(): void {}\n  set s(v: number) {}\n}\n",
    );
    assert_ne!(a.api_hash, c.api_hash);
}

#[test]
fn referenced_locals_are_part_of_the_signature() {
    let a = idx(
        "interface Local { a: number }\nconst unrelated = 1;\nexport function f(): Local { return { a: 1 }; }\n",
    );
    let b = idx(
        "interface Local { a: number }\nconst unrelated = 2;\nexport function f(): Local { return { a: 2 }; }\n",
    );
    let c = idx(
        "interface Local { a: string }\nconst unrelated = 1;\nexport function f(): Local { return { a: '' }; }\n",
    );
    assert_eq!(entry_hash(&a, "f"), entry_hash(&b, "f"));
    assert_ne!(entry_hash(&a, "f"), entry_hash(&c, "f"));
}

#[test]
fn local_export_aliases_hash_their_target() {
    for (src, changed) in [
        (
            "interface Local { x: number }\nexport { Local };\n",
            "interface Local { x: string }\nexport { Local };\n",
        ),
        (
            "interface Local { x: number }\nexport { Local as Renamed };\n",
            "interface Local { x: string }\nexport { Local as Renamed };\n",
        ),
        (
            "namespace N { export const x: number = 1; }\nexport default N;\n",
            "namespace N { export const x: string = ''; }\nexport default N;\n",
        ),
    ] {
        let (a, b) = (idx(src), idx(changed));
        let name = &a.entries[0].name;
        assert_ne!(entry_hash(&a, name), entry_hash(&b, name), "{src}");
        assert_ne!(a.api_hash, b.api_hash, "{src}");
    }
    let i = idx("import { Dep } from \"./dep\";\nexport { Dep };\n");
    assert_eq!(
        i.entry(Section::Export, "Dep").unwrap().deps,
        [Dep::Import {
            specifier: "./dep".into(),
            name: "Dep".into()
        }]
    );
}

#[test]
fn deps_record_imports_and_globals() {
    let i = idx(
        "import { Dep } from \"./dep\";\nimport type * as NS from \"./ns\";\nexport function f(a: Dep, b: NS.T): Promise<Window> { return null!; }\nexport { Other } from \"./other\";\nexport * from \"./star\";\n",
    );
    let f = i.entry(Section::Export, "f").unwrap();
    assert!(f.deps.contains(&Dep::Import {
        specifier: "./dep".into(),
        name: "Dep".into()
    }));
    assert!(f.deps.contains(&Dep::Import {
        specifier: "./ns".into(),
        name: "*".into()
    }));
    assert!(f.deps.contains(&Dep::Global("Promise".into())));
    assert!(f.deps.contains(&Dep::Global("Window".into())));
    let other = i.entry(Section::Export, "Other").unwrap();
    assert_eq!(
        other.deps,
        [Dep::Import {
            specifier: "./other".into(),
            name: "Other".into()
        }]
    );
    assert_eq!(i.export_stars, ["./star"]);
    assert_eq!(i.imports.len(), 4);
}

#[test]
fn inferred_parts_record_the_values_they_read() {
    let i = idx(
        "import { x } from \"./a\";\nconst local = helper();\nfunction helper() { return x; }\nexport const y = x;\nexport const z = local;\nexport const typed: number = x;\nexport const g = globalValue;\n",
    );
    let x = Dep::Import {
        specifier: "./a".into(),
        name: "x".into(),
    };
    assert!(i.entry(Section::Export, "y").unwrap().deps.contains(&x));
    // Through a local the initializer reads (folded into the entry).
    assert!(i.entry(Section::Export, "z").unwrap().deps.contains(&x));
    // A typed declaration's initializer is not part of its signature.
    assert!(i.entry(Section::Export, "typed").unwrap().deps.is_empty());
    assert_eq!(
        i.entry(Section::Export, "g").unwrap().deps,
        [Dep::Global("globalValue".into())]
    );
    // A changed local the initializer reads changes the entry.
    let j = idx(
        "import { x } from \"./a\";\nconst local = helper();\nfunction helper() { return String(x); }\nexport const y = x;\nexport const z = local;\nexport const typed: number = x;\nexport const g = globalValue;\n",
    );
    assert_ne!(entry_hash(&i, "z"), entry_hash(&j, "z"));
    assert_eq!(entry_hash(&i, "y"), entry_hash(&j, "y"));
}

#[test]
fn scripts_ambient_modules_and_augmentations() {
    let s = idx_of(
        "declare var g: number;\ndeclare module \"lib\" { export const x: number; }\ninterface Win { a: 1 }\n",
        "globals.d.ts",
    );
    assert!(!s.is_module);
    assert!(s.entry(Section::Global, "g").is_some());
    assert!(s.entry(Section::Global, "Win").is_some());
    assert!(s.entry(Section::AmbientModule, "lib").is_some());
    let m = idx(
        "export {};\ndeclare global { var injected: number; }\ndeclare module \"./x\" { interface X { y: 1 } }\n",
    );
    assert!(m.entry(Section::GlobalAugmentation, "injected").is_some());
    assert!(m.entry(Section::ModuleAugmentation, "./x").is_some());
}

#[test]
fn namespace_bodies_hash_declarations_only() {
    let a = idx("export namespace N { export const x: number = 1; console.log(x); }\n");
    let b = idx("export namespace N { export const x: number = 2; console.warn(x); }\n");
    let c = idx("export namespace N { export const x: string = ''; }\n");
    assert_eq!(a.api_hash, b.api_hash);
    assert_ne!(a.api_hash, c.api_hash);
}

#[test]
fn encoding_round_trips_and_rejects_damage() {
    let i = idx(
        "import { D } from \"./d\";\nexport function f(a: D): Promise<void> { return null!; }\nexport default 1;\nexport as namespace Lib;\n",
    );
    let bytes = i.encode();
    assert_eq!(DeclIndex::decode(&bytes), Some(i.clone()));
    assert_eq!(DeclIndex::decode(&bytes[..bytes.len() - 1]), None);
    let mut wrong_version = bytes.clone();
    wrong_version[4] ^= 0xff;
    assert_eq!(DeclIndex::decode(&wrong_version), None);
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(DeclIndex::decode(&trailing), None);
    // Arbitrary garbage never panics.
    for n in 0..bytes.len() {
        let _ = DeclIndex::decode(&bytes[..n]);
    }
    let _ = DeclIndex::decode(&[
        b'T', b'S', b'R', b'D', 1, 0, 0, 0, 1, 0xff, 0xff, 0xff, 0xff,
    ]);
}

#[test]
fn cache_round_trip() {
    let dir = std::env::temp_dir().join(format!("ts-r-decls-test-{}", std::process::id()));
    let cache = ts_r_cache::Cache::open_in(
        &dir,
        ts_r_decls::CACHE_NAMESPACE,
        ts_r_decls::FORMAT_VERSION,
    )
    .expect("open cache");
    let src = "export const x: number = 1;\n";
    let key = cache_key(src, SourceKind::Ts, &BindOptions::default());
    assert_ne!(
        key,
        cache_key(src, SourceKind::Dts, &BindOptions::default())
    );
    let mut computed = 0;
    let a = get_or_compute(&cache, &key, || {
        computed += 1;
        idx(src)
    });
    let b = get_or_compute(&cache, &key, || {
        computed += 1;
        idx(src)
    });
    assert_eq!(a, b);
    assert_eq!(computed, 1);
    drop(cache);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sibling_declarators_are_separate() {
    let a = idx("export const a: number = 1, b = Math.random();\n");
    let b = idx("export const a: number = 2, b = Date.now();\n");
    assert!(a.entry(Section::Export, "a").unwrap().explicit);
    assert!(!a.entry(Section::Export, "b").unwrap().explicit);
    assert_eq!(entry_hash(&a, "a"), entry_hash(&b, "a"));
    assert_ne!(entry_hash(&a, "b"), entry_hash(&b, "b"));
    let c = idx("export let a: number = 1, b = Math.random();\n");
    assert_ne!(entry_hash(&a, "a"), entry_hash(&c, "a"), "const vs let");
}

#[test]
fn script_globals_reference_each_other_as_deps() {
    let a = idx_of(
        "interface A { b: B }\ninterface B { x: number }\n",
        "g.d.ts",
    );
    let b = idx_of(
        "interface A { b: B }\ninterface B { x: string }\n",
        "g.d.ts",
    );
    let ea = a.entry(Section::Global, "A").unwrap();
    assert_eq!(ea.deps, [Dep::Global("B".into())]);
    assert_eq!(ea.hash, b.entry(Section::Global, "A").unwrap().hash);
    assert_ne!(
        a.entry(Section::Global, "B").unwrap().hash,
        b.entry(Section::Global, "B").unwrap().hash
    );
}

#[test]
fn exports_reference_sibling_exports_as_deps() {
    let src = |t: &str| {
        format!(
            "export interface A {{ x: {t} }}\nexport function f(a: A): A {{ return a; }}\n\
             export const made = new Counter(1);\n\
             export class Counter {{ constructor(n: number) {{}} }}\n\
             export default class extends Counter {{}}\nexport {{ f as g }};\n\
             export interface Self {{ next: Self }}\n"
        )
    };
    let a = idx(&src("number"));
    let b = idx(&src("string"));
    let f = a.entry(Section::Export, "f").unwrap();
    assert_eq!(f.deps, [Dep::Sibling("A".into())]);
    // A's change stays in A's entry.
    assert_eq!(entry_hash(&a, "f"), entry_hash(&b, "f"));
    assert_ne!(entry_hash(&a, "A"), entry_hash(&b, "A"));
    // Inferred from an expression; a default export.
    let made = a.entry(Section::Export, "made").unwrap();
    assert!(made.deps.contains(&Dep::Sibling("Counter".into())));
    let default = a.entry(Section::Export, "default").unwrap();
    assert!(default.deps.contains(&Dep::Sibling("Counter".into())));
    // Itself: no dependency.
    assert!(a.entry(Section::Export, "Self").unwrap().deps.is_empty());
    // The encoding keeps them.
    assert_eq!(DeclIndex::decode(&a.encode()), Some(a.clone()));
}

#[test]
fn type_only_exports_change_the_entry() {
    let a = idx("const x = 1;\nexport { x };\n");
    let b = idx("const x = 1;\nexport type { x };\n");
    let ea = a.entry(Section::Export, "x").unwrap();
    let eb = b.entry(Section::Export, "x").unwrap();
    assert!(ea.hash != eb.hash || ea.flags != eb.flags, "{ea:?} {eb:?}");
}
