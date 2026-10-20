//! `Layout::Tsc` fixtures: each `tests/tsc_layout/<name>.ts` (or `.tsx`) prints byte-for-byte as
//! the `.js` (or `.jsx`) next to it. The expected files are tsc 7's output for the input at
//! `target: esnext` (its `"use strict";` prologue removed), so every rule here is observed
//! behaviour.

use std::path::Path;

use ts_r_ast::SourceKind;
use ts_r_codegen::{CodegenOptions, Layout, PrintHints, Syntax, print_as};

fn check(name: &str, kind: SourceKind, out_ext: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsc_layout");
    let in_ext = if kind == SourceKind::Tsx { "tsx" } else { "ts" };
    let src = std::fs::read_to_string(dir.join(format!("{name}.{in_ext}"))).unwrap();
    let want = std::fs::read_to_string(dir.join(format!("{name}.{out_ext}"))).unwrap();
    let r = ts_r_parser::parse(&src, kind);
    assert!(
        !r.diagnostics.iter().any(|d| d.is_error()),
        "{name}: {:?}",
        r.diagnostics
    );
    let opts = CodegenOptions {
        layout: Layout::Tsc,
        ..CodegenOptions::default()
    };
    let got = print_as(
        &r.ast,
        &src,
        &opts,
        &PrintHints::default(),
        Syntax::JavaScript,
    )
    .code;
    assert_eq!(got, want, "{name}: Layout::Tsc output differs from tsc 7");
}

/// Function bodies keep a one-line source on one line; statement blocks and class bodies always
/// break; `else`/`catch`/`finally` start a line; embedded statements go on their own line; a
/// case's lone statement stays on the case line.
#[test]
fn blocks() {
    check("blocks", SourceKind::Ts, "js");
}

/// Object and array literals keep source line breaks after `{`/`[`, between elements and before
/// the close; patterns, arguments and parameters never break; trailing commas and holes stay.
#[test]
fn lists() {
    check("lists", SourceKind::Ts, "js");
}

/// Line breaks around binary and assignment operators, `?`/`:`, before `.` and after `,` in
/// comma expressions are kept, each adding one indentation level.
#[test]
fn operators() {
    check("operators", SourceKind::Ts, "js");
}

/// Multi-line block comments shift by the indentation change (tabs to 4-column stops, never
/// below column 0); line comments keep trailing whitespace.
#[test]
fn comments() {
    check("comments", SourceKind::Ts, "js");
}

/// Self-closing JSX elements drop the space before `/>` when they have attributes.
#[test]
fn jsx() {
    check("jsx", SourceKind::Tsx, "jsx");
}

/// Comments inside statements print around the node whose trivia holds them: `/* c */ x`
/// before, `x /* c */` after, ` // c` and a line break after a `,`. After `(`, `.`, `[`, a
/// prefix operator or `new` they trail the token (`( /* c */x`); comments touching no node
/// (`function /* c */ ()`) are dropped.
#[test]
fn expr_comments() {
    check("expr_comments", SourceKind::Ts, "js");
}

/// A comment after `{` on its line stays in statement blocks and `switch`, and is dropped in
/// function, class and multi-line object bodies; a leading block comment followed by code on
/// its line stays on that line.
#[test]
fn brace_comments() {
    check("brace_comments", SourceKind::Ts, "js");
}

/// Legacy octal (`055`) and leading-zero decimal (`09.5`) literals print as their value;
/// other literals as written. (The input has tsc's own errors, TS1121 and TS1489.)
#[test]
fn numbers() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsc_layout");
    let src = std::fs::read_to_string(dir.join("numbers.ts")).unwrap();
    let want = std::fs::read_to_string(dir.join("numbers.js")).unwrap();
    let r = ts_r_parser::parse(&src, SourceKind::Ts);
    assert!(r.diagnostics.iter().all(|d| [1121, 1489].contains(&d.code)));
    let opts = CodegenOptions {
        layout: Layout::Tsc,
        ..CodegenOptions::default()
    };
    let got = print_as(
        &r.ast,
        &src,
        &opts,
        &PrintHints::default(),
        Syntax::JavaScript,
    )
    .code;
    assert_eq!(got, want);
}
