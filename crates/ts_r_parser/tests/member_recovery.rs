ts_r_testing::cap_memory!();

use ts_r_parser::{parse, ts_r_ast::SourceKind};

#[test]
fn multiline_members_keep_keyword_operators() {
    for src in [
        "const result = obj.\nfoo as number;",
        "const result = obj.\nfoo satisfies number;",
        "if (obj.\nfoo in other) {}",
        "if (obj.\nfoo instanceof Other) {}",
        "const result = obj.\nnamespace as number;",
        "const result = obj.\nconst satisfies number;",
        "if (obj.\nvar in other) {}",
        "if (obj.\nfunction instanceof Other) {}",
    ] {
        let parsed = parse(src, SourceKind::Ts);
        assert!(
            parsed.diagnostics.is_empty(),
            "{src}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn multiline_declarations_keep_missing_member_recovery() {
    for src in [
        "obj.\nvar value = 1;",
        "obj.\nnamespace N {}",
        "obj.\nasync function f() {}",
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
