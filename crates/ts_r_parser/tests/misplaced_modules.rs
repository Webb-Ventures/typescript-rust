ts_r_testing::cap_memory!();

use std::fmt::Write;

use ts_r_parser::{parse, ts_r_ast::SourceKind};

#[test]
fn misplaced_module_keyword_errors_are_suppressed_per_declaration() {
    let source = "module A.B {}\nfunction f() { module C.D { module E.F {} } namespace G {} }\nlabel: module H.I {}\nmodule J.K {}";
    let result = parse(source, SourceKind::Ts);
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .map(|d| (d.code, &source[d.span.start as usize..d.span.end as usize]))
        .collect();
    assert_eq!(
        errors,
        [
            (1540, "A"),
            (1540, "B"),
            (1235, "module C.D { module E.F {} }"),
            (1540, "E"),
            (1540, "F"),
            (1235, "namespace G {}"),
            (1235, "module H.I {}"),
            (1540, "J"),
            (1540, "K"),
        ]
    );
}

#[test]
fn many_misplaced_modules_keep_only_placement_errors() {
    const COUNT: usize = 10_000;
    for keyword in ["namespace", "module"] {
        let mut source = String::from("function f() {");
        for i in 0..COUNT {
            write!(source, "{keyword} N{i} {{}}").unwrap();
        }
        source.push('}');
        let result = parse(&source, SourceKind::Ts);
        assert_eq!(result.diagnostics.len(), COUNT, "{keyword}");
        assert!(result.diagnostics.iter().all(|d| d.code == 1235));
    }
}
