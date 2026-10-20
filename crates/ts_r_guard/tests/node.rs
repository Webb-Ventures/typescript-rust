//! Generates validators for every `Cases` property of `fixtures/types.ts`, runs them in node
//! against `fixtures/vectors.mjs` (under `scripts/limit.sh`), and checks every `Unsupported`
//! property is rejected with its reason. The node part is skipped when `node` is missing.

use std::path::{Path, PathBuf};

use ts_r_ast::{Node, NodeId};
use ts_r_checker::{ProgramFiles, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_guard::{Generator, GuardOptions};

ts_r_testing::cap_memory!();

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn program() -> ProgramFiles {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"noEmit":true},"include":["*.ts"]}"#,
    );
    for name in ["types.ts", "other.ts", "reexport.ts", "ambient.d.ts"] {
        let text = std::fs::read_to_string(fixtures().join(name)).unwrap_or_default();
        fs.add_file(&format!("/p/{name}"), &text);
    }
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    load_program(&fs, &cfg.file_names, &cfg.options, None, 1)
}

/// `(key, type annotation)` of each property of interface `name` in `types.ts`.
fn props(p: &ProgramFiles, name: &str) -> (usize, Vec<(String, NodeId)>) {
    let file = p
        .files
        .iter()
        .position(|f| f.path == "/p/types.ts")
        .expect("types.ts loaded");
    let ast = &p.files[file].ast;
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return (file, Vec::new());
    };
    for &s in ast.list(stmts) {
        let Node::Interface { name: n, body, .. } = *ast.node(s) else {
            continue;
        };
        if !matches!(*ast.node(n), Node::Ident { name: a } if ast.atom(a) == name) {
            continue;
        }
        let mut out = Vec::new();
        for &m in ast.list(body) {
            if let Node::PropertySignature {
                key,
                type_ann: Some(t),
                ..
            } = *ast.node(m)
            {
                out.push((
                    ts_r_checker::nodes::key_text(ast, key).unwrap_or_default(),
                    t,
                ));
            }
        }
        return (file, out);
    }
    (file, Vec::new())
}

#[test]
fn unsupported_types_have_reasons() {
    let p = program();
    let (file, cases) = props(&p, "Unsupported");
    assert_eq!(cases.len(), 10);
    let mut g = Generator::new(&p, GuardOptions::default());
    let expected = [
        ("fn", "function types have no runtime shape"),
        ("anyT", "'any' has no shape to check"),
        ("promise", "a promise can't be checked synchronously"),
        ("cls", "class 'Klass' is not exported from its module"),
        ("method", "method 'm' is a function"),
        ("cond", "conditional types"),
        ("query", "'typeof' types need the checker"),
        ("brand", "is a branded type; name it with a type alias"),
        ("never", "no value has type 'never'"),
        ("missing", "'NotDeclaredAnywhere' is not declared"),
    ];
    for ((key, ty), (k, reason)) in cases.iter().zip(expected) {
        assert_eq!(key, k);
        let err = g.add(file, *ty).expect_err(key);
        assert!(
            err.reason.contains(reason),
            "{key}: reason {:?} lacks {reason:?}",
            err.reason
        );
    }
    // Failed types leave nothing behind.
    assert!(g.code().is_empty(), "{}", g.code());
}

#[test]
fn validators_run_in_node() {
    let p = program();
    let (file, cases) = props(&p, "Cases");
    assert!(cases.len() >= 30);
    let mut g = Generator::new(&p, GuardOptions::default());
    let mut map = Vec::new();
    for (key, ty) in &cases {
        match g.add(file, *ty) {
            Ok(e) => map.push(format!("{key:?}: {:?}", e.entry)),
            Err(u) => panic!("{key}: {} unsupported: {}", u.ty, u.reason),
        }
    }
    let module = g.module();
    // Deterministic.
    let mut g2 = Generator::new(&p, GuardOptions::default());
    for (_, ty) in &cases {
        let _ = g2.add(file, *ty);
    }
    assert_eq!(module, g2.module());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ts_r_guard_node");
    let _ = std::fs::create_dir_all(&dir);
    let gen_path = dir.join("generated.mjs");
    let map_path = dir.join("map.json");
    std::fs::write(&gen_path, &module).expect("write module");
    std::fs::write(&map_path, format!("{{{}}}", map.join(", "))).expect("write map");
    let out = ts_r_testing::limited("node", 1024, 60)
        .arg(fixtures().join("driver.mjs"))
        .arg(&gen_path)
        .arg(&map_path)
        .output();
    let Ok(out) = out else {
        eprintln!("skipped: could not run node");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && stderr.contains("node: command not found") {
        eprintln!("skipped: node not found");
        return;
    }
    assert!(out.status.success(), "node failed:\n{stdout}\n{stderr}");
    let line = stdout.lines().last().unwrap_or("");
    assert!(
        line.contains("\"fails\":[]"),
        "failures:\n{}",
        line.replace("\\n", "\n")
    );
    eprintln!("node: {line} ({} bytes of generated code)", module.len());
}
