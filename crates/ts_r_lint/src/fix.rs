//! Applying fixes safely: sorted by range, non-overlapping, one pass. The caller re-lints and
//! repeats (see [`crate::engine::lint_and_fix`]).

use crate::diagnostic::Fix;

/// Applies as many of `fixes` as possible in one pass. A fix is skipped when its range starts
/// at or before the end of the previously applied fix (ESLint's rule, so touching fixes are
/// applied in later passes). Returns the new text and the number of fixes applied.
pub fn apply_fixes(src: &str, fixes: &[&Fix]) -> (String, usize) {
    let mut merged: Vec<crate::diagnostic::Edit> = fixes
        .iter()
        .filter(|f| !f.edits.is_empty())
        .map(|f| f.merged(src))
        .filter(|e| e.span.start <= e.span.end && (e.span.end as usize) <= src.len())
        .collect();
    merged.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = String::with_capacity(src.len());
    let mut last: Option<u32> = None;
    let mut pos = 0u32;
    let mut applied = 0;
    for e in &merged {
        if let Some(l) = last
            && l >= e.span.start
        {
            continue;
        }
        if !src.is_char_boundary(e.span.start as usize)
            || !src.is_char_boundary(e.span.end as usize)
        {
            continue;
        }
        out.push_str(&src[pos as usize..e.span.start as usize]);
        out.push_str(&e.text);
        pos = e.span.end;
        last = Some(e.span.end);
        applied += 1;
    }
    out.push_str(&src[pos as usize..]);
    (out, applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_span::Span;

    #[test]
    fn non_overlapping_in_order() {
        let a = Fix::replace(Span::new(0, 1), "A");
        let b = Fix::replace(Span::new(1, 2), "B"); // touches a: skipped this pass
        let c = Fix::replace(Span::new(3, 4), "C");
        let (out, n) = apply_fixes("abcd", &[&c, &b, &a]);
        assert_eq!(out, "AbcC");
        assert_eq!(n, 2);
    }

    #[test]
    fn multi_edit_fix_is_one_unit() {
        let f = Fix::replace(Span::new(0, 1), "X").and(Span::new(3, 4), "Y");
        let g = Fix::replace(Span::new(2, 3), "Z"); // inside f's range: skipped
        let (out, n) = apply_fixes("abcd", &[&f, &g]);
        assert_eq!(out, "XbcY");
        assert_eq!(n, 1);
    }
}
