ts_r_testing::cap_memory!();

use std::fmt::Write;
use ts_r_parser::{parse, ts_r_ast::SourceKind, without_grammar};

#[test]
fn grammar_keeps_only_the_diagnostic_surviving_positional_deduplication() {
    let parsed = parse(
        "class C { constructor(public ...a: any[], b: any) {} }",
        SourceKind::Ts,
    );
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    assert_eq!(parsed.diagnostics[0].code, 1014);
    assert_eq!(parsed.grammar, parsed.diagnostics);
    assert!(parsed.syntax_errors().is_empty());
}

#[test]
fn grammar_membership_compares_diagnostics_at_the_same_position() {
    let parsed = parse("const a;", SourceKind::Ts);
    let mut syntax = parsed.diagnostics[0].clone();
    syntax.code = 1003;
    let mut diagnostics = parsed.diagnostics.clone();
    diagnostics.push(syntax.clone());
    assert_eq!(without_grammar(&diagnostics, &parsed.grammar), [syntax]);
}

#[test]
fn many_deferred_diagnostics_can_be_separated() {
    let mut src = String::new();
    for i in 0..100_000 {
        writeln!(src, "const a{i};").unwrap();
    }
    let parsed = parse(&src, SourceKind::Ts);
    assert_eq!(parsed.diagnostics.len(), 100_000);
    assert_eq!(parsed.grammar, parsed.diagnostics);
    assert!(parsed.syntax_errors().is_empty());
}
