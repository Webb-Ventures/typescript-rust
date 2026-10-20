//! Character classes.

/// Bit 0: ASCII identifier start. Bit 1: ASCII identifier part.
static ASCII_IDENT: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut i = 0;
    while i < 128 {
        let c = i as u8;
        let start = c.is_ascii_alphabetic() || c == b'_' || c == b'$';
        let part = start || c.is_ascii_digit();
        t[i] = (start as u8) | ((part as u8) << 1);
        i += 1;
    }
    t
};

#[inline]
pub fn is_ascii_ident_start(b: u8) -> bool {
    b < 128 && ASCII_IDENT[b as usize] & 1 != 0
}

#[inline]
pub fn is_ascii_ident_part(b: u8) -> bool {
    b < 128 && ASCII_IDENT[b as usize] & 2 != 0
}

#[inline]
pub fn is_id_start(c: char) -> bool {
    if c.is_ascii() {
        is_ascii_ident_start(c as u8)
    } else {
        unicode_ident::is_xid_start(c) || is_other_id_start(c)
    }
}

#[inline]
pub fn is_id_part(c: char) -> bool {
    if c.is_ascii() {
        is_ascii_ident_part(c as u8)
    } else {
        unicode_ident::is_xid_continue(c)
            || is_other_id_start(c)
            || c == '\u{200C}'
            || c == '\u{200D}'
    }
}

/// Characters that ECMAScript's ID_Start includes but XID_Start does not (they are not
/// NFKC-stable), plus Other_ID_Start.
#[inline]
fn is_other_id_start(c: char) -> bool {
    matches!(
        c,
        '\u{037A}' | '\u{0E33}' | '\u{0EB3}' | '\u{309B}' | '\u{309C}' | '\u{FC5E}'
            ..='\u{FC63}'
                | '\u{FDFA}'
                | '\u{FDFB}'
                | '\u{FE70}'
                | '\u{FE72}'
                | '\u{FE74}'
                | '\u{FE76}'
                | '\u{FE78}'
                | '\u{FE7A}'
                | '\u{FE7C}'
                | '\u{FE7E}'
                | '\u{FF9E}'
                | '\u{FF9F}'
                | '\u{1885}'
                | '\u{1886}'
                | '\u{2118}'
                | '\u{212E}'
    )
}

/// ECMAScript WhiteSpace other than ASCII space/tab/VT/FF, plus U+0085 (NEXT LINE), which
/// tsc also skips as a space (fileWithNextLine2).
#[inline]
pub fn is_unicode_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0085}' | '\u{00A0}' | '\u{FEFF}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

#[inline]
pub fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert!(is_id_start('a') && is_id_start('$') && is_id_start('_') && !is_id_start('1'));
        assert!(is_id_part('1') && !is_id_part('-'));
        assert!(is_id_start('ä') && is_id_start('π') && is_id_start('中'));
        assert!(is_id_part('\u{200D}') && !is_id_start('\u{200D}'));
        assert!(is_id_start('\u{2118}'));
        assert!(is_unicode_whitespace('\u{00A0}') && !is_unicode_whitespace('a'));
        assert!(is_line_terminator('\u{2028}'));
    }
}
