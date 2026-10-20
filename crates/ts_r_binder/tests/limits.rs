//! Pathological inputs: the binder must stop at its depth limit with TS99001 instead of
//! overflowing the stack, and must stay linear on wide inputs.

ts_r_testing::cap_memory!();

use ts_r_ast::{Ast, Node, SourceKind, Span};
use ts_r_binder::{BindOptions, bind};

/// An AST deeper than the parser would ever build: 200k nested parentheses.
#[test]
fn deep_ast_reports_depth_limit() {
    let mut ast = Ast::new(SourceKind::Ts, 0);
    let mut e = ast.add(Node::Missing {}, Span::new(0, 0));
    for _ in 0..200_000 {
        e = ast.add(Node::Paren { expr: e }, Span::new(0, 0));
    }
    let stmt = ast.add(Node::ExprStmt { expr: e }, Span::new(0, 0));
    let stmts = ast.add_list(&[stmt]);
    ast.root = ast.add(Node::Program { stmts }, Span::new(0, 0));
    let b = bind(&ast, "", &BindOptions::default());
    assert!(
        b.diagnostics.iter().any(|d| d.code == 99001),
        "{:?}",
        b.diagnostics
    );
}

/// Deep but parseable nesting of every kind the binder recurses through.
#[test]
fn deep_parsed_nesting_terminates() {
    let n = 2_000;
    let cases = [
        format!("{}{}", "{".repeat(n), "}".repeat(n)),
        format!(
            "namespace A {{{}}}",
            "namespace B {".repeat(n) + &"}".repeat(n)
        ),
        format!("x = {}1{};", "(".repeat(n), ")".repeat(n)),
        format!("{}0{}", "() => ".repeat(n), ""),
        format!("{}x{}", "if (a) ".repeat(n), ";"),
        format!("type T = {}number{};", "Array<".repeat(n), ">".repeat(n)),
    ];
    for src in &cases {
        let p = ts_r_parser::parse(src, SourceKind::Ts);
        let _ = bind(&p.ast, src, &BindOptions::default());
    }
}

/// Wide inputs: many declarations of one name, many labels, long else-if chains.
#[test]
fn wide_inputs_are_linear() {
    let start = std::time::Instant::now();
    let dup = "let x = 1;\n".repeat(20_000);
    let p = ts_r_parser::parse(&dup, SourceKind::Ts);
    let b = bind(&p.ast, &dup, &BindOptions::default());
    assert_eq!(b.diagnostics.len(), 20_000);
    let chain = format!(
        "function f(a: number) {{ {} }}",
        "if (a === 1) return 1; else ".repeat(10_000) + "return 0;"
    );
    let p = ts_r_parser::parse(&chain, SourceKind::Ts);
    let _ = bind(&p.ast, &chain, &BindOptions::default());
    let iface = format!(
        "interface I {{ {} }}",
        (0..20_000)
            .map(|i| format!("m{i}: number;"))
            .collect::<String>()
    );
    let p = ts_r_parser::parse(&iface, SourceKind::Ts);
    let _ = bind(&p.ast, &iface, &BindOptions::default());
    assert!(start.elapsed().as_secs() < 20, "took {:?}", start.elapsed());
}

/// Namespaces that re-export themselves (or each other) under many names: classifying their
/// instantiation explores each namespace once per lookup depth, not once per path.
#[test]
fn self_reexporting_namespaces_are_classified_quickly() {
    let start = std::time::Instant::now();
    let aliases = |target: &str| {
        (0..20)
            .map(|i| format!("{target} as a{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let cyclic = format!("namespace A {{ export {{ {} }}; }}", aliases("A"));
    let chain = (0..10)
        .map(|i| {
            format!(
                "namespace N{i} {{ export {{ {} }}; }}\n",
                aliases(&format!("N{}", i + 1))
            )
        })
        .collect::<String>();
    for src in [&cyclic, &chain] {
        let p = ts_r_parser::parse(src, SourceKind::Ts);
        let _ = bind(&p.ast, src, &BindOptions::default());
    }
    assert!(start.elapsed().as_secs() < 20, "took {:?}", start.elapsed());
}

/// `export default` of a 200k-segment member chain: the parser builds it iteratively, so the
/// binder's entity-name check must not recurse down it.
#[test]
fn long_export_default_member_chain_terminates() {
    let src = format!("export default x{};", ".a".repeat(200_000));
    let p = ts_r_parser::parse(&src, SourceKind::Ts);
    let _ = bind(&p.ast, &src, &BindOptions::default());
}
