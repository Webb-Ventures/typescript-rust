//! Symbol names: file atoms, reserved internal names and binder-synthesized strings.

use ts_r_ast::{Ast, Atom};

/// A symbol name within one file.
///
/// - `< RESERVED_BASE`: an [`Atom`] of the file's AST.
/// - `RESERVED_BASE..EXTRA_BASE`: a reserved internal name that no source text can spell
///   (`__call`, `export=`, ...).
/// - `>= EXTRA_BASE`: an entry of [`Names::extra`] (strings the binder had to create, such as
///   canonical numeric property names that do not occur as atoms).
///
/// Equal strings always map to equal names within a file (see [`Names::name_for`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Name(pub u32);

macro_rules! reserved {
    ($($id:ident = $n:literal $text:literal;)*) => {
        impl Name {
            $( pub const $id: Name = Name(Name::RESERVED_BASE + $n); )*
        }
        fn reserved_text(n: u32) -> &'static str {
            match n { $( $n => $text, )* _ => "__unknown" }
        }
    };
}

reserved! {
    CALL = 0 "__call";
    NEW = 1 "__new";
    INDEX = 2 "__index";
    CONSTRUCTOR = 3 "__constructor";
    EXPORT_EQUALS = 4 "export=";
    EXPORT_STAR = 5 "__export";
    COMPUTED = 6 "__computed";
    MISSING = 7 "(Missing)";
    GLOBAL = 8 "__global";
    CLASS = 9 "__class";
    FUNCTION = 10 "__function";
    OBJECT = 11 "__object";
    TYPE = 12 "__type";
    FILE = 13 "__file";
    THIS = 14 "this";
}

impl Name {
    pub const RESERVED_BASE: u32 = 0x8000_0000;
    pub const EXTRA_BASE: u32 = 0x8000_1000;

    #[inline]
    pub fn from_atom(atom: Atom) -> Name {
        Name(atom.0)
    }

    #[inline]
    pub fn is_reserved(self) -> bool {
        self.0 >= Name::RESERVED_BASE && self.0 < Name::EXTRA_BASE
    }

    /// The atom, when this name is one.
    #[inline]
    pub fn atom(self) -> Option<Atom> {
        (self.0 < Name::RESERVED_BASE).then_some(Atom(self.0))
    }
}

/// Strings the binder created plus lookup helpers. Owned by the `BoundFile`.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub extra: Vec<String>,
    lookup: rustc_hash::FxHashMap<String, u32>,
}

impl Names {
    /// The name for `s`: an existing atom of the file if there is one, else an extra entry.
    pub fn name_for(&mut self, ast: &Ast, s: &str) -> Name {
        if let Some(a) = ast.atoms.find(s) {
            return Name::from_atom(a);
        }
        if let Some(&i) = self.lookup.get(s) {
            return Name(Name::EXTRA_BASE + i);
        }
        let i = self.extra.len() as u32;
        self.extra.push(s.to_owned());
        self.lookup.insert(s.to_owned(), i);
        Name(Name::EXTRA_BASE + i)
    }

    /// Finds the name for `s` without creating one.
    pub fn find(&self, ast: &Ast, s: &str) -> Option<Name> {
        // The one reserved name looked up by text. Source can spell it only as a string
        // (`ExportEquals = "export="` in typescript.d.ts), which is never a module export, so
        // it is checked before the file's atoms.
        if s == "export=" {
            return Some(Name::EXPORT_EQUALS);
        }
        if let Some(a) = ast.atoms.find(s) {
            return Some(Name::from_atom(a));
        }
        self.lookup.get(s).map(|&i| Name(Name::EXTRA_BASE + i))
    }

    /// The text of a name. Reserved names print as tsc's internal spelling.
    pub fn text<'a>(&'a self, ast: &'a Ast, name: Name) -> &'a str {
        if let Some(a) = name.atom() {
            ast.atom(a)
        } else if name.0 >= Name::EXTRA_BASE {
            self.extra
                .get((name.0 - Name::EXTRA_BASE) as usize)
                .map_or("__unknown", |s| s.as_str())
        } else {
            reserved_text(name.0 - Name::RESERVED_BASE)
        }
    }
}

/// JavaScript `Number::toString` for property names (`0x10` → `"16"`, `1.50` → `"1.5"`,
/// `1e21` → `"1e+21"`).
pub fn js_number_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    let neg = v < 0.0;
    // Shortest round-trip digits in scientific form, e.g. "1.2345e3".
    let sci = format!("{:e}", v.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if n > 0 { '+' } else { '-' });
        out.push_str(&(n - 1).abs().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_names() {
        assert_eq!(js_number_to_string(16.0), "16");
        assert_eq!(js_number_to_string(1.5), "1.5");
        assert_eq!(js_number_to_string(0.000001), "0.000001");
        assert_eq!(js_number_to_string(0.0000001), "1e-7");
        assert_eq!(js_number_to_string(1e21), "1e+21");
        assert_eq!(
            js_number_to_string(123456789012345680000.0),
            "123456789012345680000"
        );
        assert_eq!(js_number_to_string(-2.0), "-2");
        assert_eq!(js_number_to_string(0.1), "0.1");
        assert_eq!(js_number_to_string(1.25e-10), "1.25e-10");
    }
}
