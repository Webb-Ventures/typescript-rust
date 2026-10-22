//! Forced compiler flags (S-04, TSR0010, TSR0011). Sound and strict files are checked with
//! `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes` and
//! `isolatedDeclarations` on. The sound pass runs its own checker over the program with those
//! options (only sound and strict files are checked by it), and a diagnostic it reports that
//! the regular check did not is the forced flag's: it is re-coded as TSR with tsc's message
//! wrapped (record: "Forced flags"). Diagnostics both checks report keep their TS code.

use ts_r_checker::program::ProgramFiles;
use ts_r_config::{OptionId, OptionValue, StrictFlag};
use ts_r_diagnostics::Diagnostic;

use crate::codes::{self, SoundMessage};
use crate::pragma::Raw;

const STRICT_FLAGS: &[(StrictFlag, OptionId, &str)] = &[
    (
        StrictFlag::NoImplicitAny,
        OptionId::NoImplicitAny,
        "noImplicitAny",
    ),
    (
        StrictFlag::NoImplicitThis,
        OptionId::NoImplicitThis,
        "noImplicitThis",
    ),
    (
        StrictFlag::StrictNullChecks,
        OptionId::StrictNullChecks,
        "strictNullChecks",
    ),
    (
        StrictFlag::StrictFunctionTypes,
        OptionId::StrictFunctionTypes,
        "strictFunctionTypes",
    ),
    (
        StrictFlag::StrictBindCallApply,
        OptionId::StrictBindCallApply,
        "strictBindCallApply",
    ),
    (
        StrictFlag::StrictPropertyInitialization,
        OptionId::StrictPropertyInitialization,
        "strictPropertyInitialization",
    ),
    (
        StrictFlag::StrictBuiltinIteratorReturn,
        OptionId::StrictBuiltinIteratorReturn,
        "strictBuiltinIteratorReturn",
    ),
    (
        StrictFlag::UseUnknownInCatchVariables,
        OptionId::UseUnknownInCatchVariables,
        "useUnknownInCatchVariables",
    ),
];

/// tsc codes that only `exactOptionalPropertyTypes` produces.
const EXACT_OPTIONAL_CODES: &[u32] = &[2375, 2379, 2412];

/// Which forced flags the project did not already have on.
#[derive(Clone, Debug, Default)]
pub struct Newly {
    pub strict: Vec<&'static str>,
    pub unchecked_index: bool,
    pub exact_optional: bool,
    pub isolated_declarations: bool,
}

/// The programs the sound pass checks with: every forced flag on (`all`), and when the
/// strict family is newly forced, an intermediate with only that (to tell TSR0011 from S-04).
#[derive(Debug)]
pub struct Forced {
    pub all: ProgramFiles,
    pub strict_only: Option<ProgramFiles>,
    pub newly: Newly,
}

fn copy_program(p: &ProgramFiles) -> ProgramFiles {
    ProgramFiles {
        files: p.files.clone(),
        resolutions: p.resolutions.clone(),
        mode_resolutions: p.mode_resolutions.clone(),
        options: p.options.clone(),
        roots: p.roots.clone(),
        missing_libs: p.missing_libs.clone(),
        missing_types: p.missing_types.clone(),
        missing_type_references: p.missing_type_references.clone(),
        ambiguous_roots: p.ambiguous_roots.clone(),
        deps: p.deps.clone(),
    }
}

pub fn forced_programs(p: &ProgramFiles) -> Forced {
    let o = &p.options;
    let e = o.effective();
    let mut newly = Newly::default();
    for &(f, _, name) in STRICT_FLAGS {
        if !e.strict_flag(f) {
            newly.strict.push(name);
        }
    }
    newly.unchecked_index = o.no_unchecked_indexed_access() != Some(true);
    newly.exact_optional = o.exact_optional_property_types() != Some(true);
    newly.isolated_declarations = o.isolated_declarations() != Some(true);
    let set_strict = |opts: &mut ts_r_config::CompilerOptions| {
        opts.set(OptionId::Strict, Some(OptionValue::Bool(true)), None);
        for &(_, id, _) in STRICT_FLAGS {
            opts.set(id, Some(OptionValue::Bool(true)), None);
        }
    };
    let strict_only = (!newly.strict.is_empty()).then(|| {
        let mut q = copy_program(p);
        set_strict(&mut q.options);
        q
    });
    let mut all = copy_program(p);
    set_strict(&mut all.options);
    for id in [
        OptionId::NoUncheckedIndexedAccess,
        OptionId::IsolatedDeclarations,
    ] {
        all.options.set(id, Some(OptionValue::Bool(true)), None);
    }
    all.options.set(
        OptionId::ExactOptionalPropertyTypes,
        Some(OptionValue::Bool(true)),
        None,
    );
    Forced {
        all,
        strict_only,
        newly,
    }
}

fn key(d: &Diagnostic) -> (u32, u32, u32) {
    (d.span.start, d.span.end, d.code)
}

/// tsc's message with the sound-mode note on its first line (chain lines follow).
fn wrap(m: SoundMessage, tsc: &str, extra: &str) -> String {
    let (first, rest) = match tsc.find('\n') {
        Some(i) => (&tsc[..i], &tsc[i..]),
        None => (tsc, ""),
    };
    let mut s = m.format(&[first, extra]);
    s.push_str(rest);
    s
}

/// Name of the optional property an `exactOptionalPropertyTypes` error is about, if the
/// message chain names one.
fn property_in(message: &str) -> String {
    for marker in ["property '", "Property '"] {
        if let Some(i) = message.rfind(marker) {
            let rest = &message[i + marker.len()..];
            if let Some(j) = rest.find('\'') {
                return rest[..j].to_owned();
            }
        }
    }
    "p".to_owned()
}

/// The diagnostics of one file that come from forced flags: re-coded (`Raw`) or, under
/// `forced-flag-codes = "TS"` (open question 18), kept with tsc's code.
pub fn recode(
    base: &[&Diagnostic],
    strict_only: Option<&[Diagnostic]>,
    all: &[Diagnostic],
    newly: &Newly,
    keep_tsc_codes: bool,
) -> (Vec<Raw>, Vec<Diagnostic>) {
    let mut seen: rustc_hash::FxHashSet<(u32, u32, u32)> = base.iter().map(|d| key(d)).collect();
    let mut raws = Vec::new();
    let mut kept = Vec::new();
    let mut push = |d: &Diagnostic, m: SoundMessage, extra: &str| {
        if keep_tsc_codes {
            kept.push(d.clone());
        } else {
            let mut r = Raw::new(m, d.span, &[]);
            r.text = wrap(m, &d.message, extra);
            raws.push(r);
        }
    };
    if let Some(s) = strict_only {
        let flag = if newly.strict.len() == 1 {
            newly.strict[0]
        } else {
            "strict"
        };
        for d in s {
            if d.is_error() && seen.insert(key(d)) {
                push(d, codes::FORCED_STRICT_FLAG, flag);
            }
        }
    }
    for d in all {
        if !d.is_error() || !seen.insert(key(d)) {
            continue;
        }
        if (9005..=9039).contains(&d.code) && newly.isolated_declarations {
            push(d, codes::FORCED_ISOLATED_DECLARATIONS, "");
        } else if EXACT_OPTIONAL_CODES.contains(&d.code) && newly.exact_optional {
            push(d, codes::FORCED_EXACT_OPTIONAL, &property_in(&d.message));
        } else if newly.unchecked_index {
            push(d, codes::FORCED_UNCHECKED_INDEX, "");
        } else if newly.exact_optional {
            push(d, codes::FORCED_EXACT_OPTIONAL, &property_in(&d.message));
        } else if !newly.strict.is_empty() {
            push(d, codes::FORCED_STRICT_FLAG, "strict");
        }
    }
    (raws, kept)
}
