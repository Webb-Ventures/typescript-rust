//! Queries editors and type-aware lint rules make after a file is checked: the type and
//! contextual type of an expression (expectations from tsc 7.0.2's `getTypeAtLocation`
//! and `getContextualType`, through typescript-eslint's rule tests).

use ts_r_ast::NodeId;
use ts_r_checker::{Checker, Ck, LibStore, Loc, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

/// Checks `src` as `/p/a.ts` (strict) and returns, for each expression written right after
/// a `/*?*/` marker (the outermost one starting there; `/*?s*/`: the innermost), its text,
/// type and contextual type.
fn marked(src: &str) -> Option<Vec<(String, String, Option<String>)>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022"}, "files": ["a.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let a = program.file_index("/p/a.ts")?;
    let file = FileId(a as u32);
    let ast = &program.files[a].ast;
    let checker = Checker::new(&program);
    let mut ck = Ck::new(&checker, file);
    let _ = ck.check_source_file(file);
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(k) = src[pos..].find("/*?") {
        let small = src[pos + k..].starts_with("/*?s*/");
        let start = pos + k + if small { 6 } else { 5 };
        pos = start;
        let mut best: Option<(NodeId, u32)> = None;
        for raw in 1..ast.node_count() as u32 {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            let sp = ast.span(id);
            if sp.start as usize != start
                || matches!(
                    ast.node(id),
                    ts_r_ast::Node::ExprStmt { .. } | ts_r_ast::Node::VarDecl { .. }
                )
            {
                continue;
            }
            if best.is_none_or(|(_, e)| if small { sp.end < e } else { sp.end > e }) {
                best = Some((id, sp.end));
            }
        }
        let (id, end) = best?;
        let l = Loc::new(file, id);
        let t = ck.check_expr(l);
        let ts = ck.type_to_string(t);
        let ctx = ck.contextual_type(l).map(|c| ck.type_to_string(c));
        out.push((src[start..end as usize].to_owned(), ts, ctx));
    }
    Some(out)
}

/// After a call is resolved, a callback's return expression is contextually typed by the
/// resolved signature, inferences applied (no-unnecessary-type-assertion's
/// `[].map(() => <T>{...})`).
#[test]
fn resolved_call_argument_context() {
    let Some(out) = marked("[].map(() => /*?*/<{ a: false; b: false }>{ a: false, b: false });\n")
    else {
        return;
    };
    assert_eq!(out[0].2.as_deref(), Some("{ a: false; b: false; }"));
}

/// An array literal that is a destructuring assignment target is a tuple
/// (no-unsafe-assignment's `[[[[x]]]] = [1 as any]`).
#[test]
fn destructuring_target_array_literal_is_tuple() {
    let Some(out) = marked("declare let x: any;\n/*?s*/[[[[x]]]] = [1 as any];\n") else {
        return;
    };
    assert_eq!(out[0].1, "[[[[any]]]]");
}

/// `new.target` is the constructor's class, also inside an arrow.
#[test]
fn new_target_type() {
    let Some(out) = marked(
        "class Foo { constructor() { /*?*/new.target; const g = () => /*?*/new.target; } }\n\
         function F() { /*?*/new.target; }\n",
    ) else {
        return;
    };
    assert_eq!(out[0].1, "typeof Foo");
    assert_eq!(out[1].1, "typeof Foo");
    assert_eq!(out[2].1, "() => void");
}

/// A property name reads index signatures keyed by a string mapping it fits
/// (no-unnecessary-condition's `t.a?.a` with `[name: Lowercase<string>]`).
#[test]
fn pattern_index_signature_property_types() {
    let Some(out) = marked(
        "interface T { [k: Lowercase<string>]: { [k: Lowercase<string>]: { v: 1 } } }\n\
         declare const t: T;\n/*?*/t.a;\n/*?*/t.a?.b;\n/*?*/t.a?.b.v;\n",
    ) else {
        return;
    };
    assert_eq!(out[0].1, "{ [k: Lowercase<string>]: { v: 1; }; }");
    assert_eq!(out[1].1, "{ v: 1; }");
    assert_eq!(out[2].1, "1");
}
