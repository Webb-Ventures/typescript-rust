ts_r_testing::cap_memory!();

use ts_r_parser::{parse, ts_r_ast::SourceKind};

#[test]
fn multiline_declarations_keep_missing_member_recovery() {
    for src in [
        "obj.\nvar value = 1;",
        "obj.\nnamespace N {}",
        "obj.\nasync function f() {}",
        // tsc: a name followed by another name on the same line starts a new statement.
        "obj.\nfoo as number;",
        "obj.\nfoo in other;",
        "obj.\nnew Bar().x;",
    ] {
        let parsed = parse(src, SourceKind::Ts);
        assert_eq!(
            parsed.diagnostics.len(),
            1,
            "{src}: {:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.diagnostics[0].code, 1003, "{src}");
        assert_eq!(parsed.diagnostics[0].span.start, 4, "{src}");
    }
}
