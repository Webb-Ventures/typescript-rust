//! `Layout::Tsc` fixtures: each `tests/tsc_layout/<name>.ts` (or `.tsx`) prints byte-for-byte as
//! the `.js` (or `.jsx`) next to it. The expected files are tsc 7's output for the input at
//! `target: esnext` (its `"use strict";` prologue removed), so every rule here is observed
//! behaviour.

use std::path::Path;

use ts_r_ast::SourceKind;
use ts_r_codegen::{CodegenOptions, Layout, PrintHints, Syntax, print_as};

fn check(name: &str, kind: SourceKind, out_ext: &str) {
    check_parsed(name, kind, out_ext, false);
}

/// [`check`] for inputs with syntax errors: tsc prints what its parser recovered.
fn check_recovered(name: &str, kind: SourceKind, out_ext: &str) {
    check_parsed(name, kind, out_ext, true);
}

fn check_parsed(name: &str, kind: SourceKind, out_ext: &str, errors: bool) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsc_layout");
    let in_ext = if kind == SourceKind::Tsx { "tsx" } else { "ts" };
    let src = std::fs::read_to_string(dir.join(format!("{name}.{in_ext}"))).unwrap();
    let want = std::fs::read_to_string(dir.join(format!("{name}.{out_ext}"))).unwrap();
    let r = ts_r_parser::parse(&src, kind);
    assert_eq!(
        r.diagnostics.iter().any(|d| d.is_error()),
        errors,
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

/// Error recovery in JSX, as tsc prints it: an empty `{}` prints nothing, a missing attribute
/// value leaves the bare name, an unclosed element gets `</>`, and children go on after an
/// expression container that lacks its `}`.
#[test]
fn jsx_recovery() {
    check_recovered("jsx_recovery", SourceKind::Tsx, "jsx");
    check_recovered("jsx_unclosed", SourceKind::Tsx, "jsx");
}

/// A statement that starts at a binary operator (its left operand missing, TS1109) keeps the
/// space before the operator, or is indented when the missing operand sits at the end of the
/// line before (`** 2;` on line 2); a bare `super` is `super.` and its missing name.
#[test]
fn missing_operand() {
    check_recovered("missing_operand", SourceKind::Ts, "js");
}

/// Comments between the tokens of import and export clauses stay where they are written
/// (tsc 7.0.2, importExportInternalComments).
#[test]
fn clause_comments() {
    check("clause_comments", SourceKind::Ts, "js");
}

/// Comments on the lines before an element access's `[` and `]` and a parenthesized
/// expression's `)` lead those tokens (tsc 7.0.2, elementAccessExpressionInternalComments,
/// parenthesizedExpressionInternalComments).
#[test]
fn access_comments() {
    check("access_comments", SourceKind::Ts, "js");
}

/// A `;` between object literal members is recovered as a `,`, the last one too
/// (tsc 7.0.2, incompleteObjectLiteral1, objectLiteralWithSemicolons5).
#[test]
fn object_semicolons() {
    check_recovered("object_semicolons", SourceKind::Ts, "js");
}

/// `1[0]` keeps no dot; `088e4.x` is written by value (`880000..x`).
#[test]
fn numeric_objects() {
    check_recovered("numeric_objects", SourceKind::Ts, "js");
}
