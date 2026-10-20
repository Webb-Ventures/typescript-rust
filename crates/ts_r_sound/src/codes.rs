//! Sound-mode diagnostic codes and messages (design record 0007). A code `TSRrrii` is stored
//! as `SOUND_CODE_BASE + rrii`: `rr` is the rule (00 general, 01..14 the rules, 15
//! `parse<T>` generation), `ii` the item. Every sound diagnostic is an error.

use ts_r_diagnostics::{Category, Diagnostic, FileId, SOUND_CODE_BASE};
use ts_r_span::Span;

/// One sound-mode message: its number (`201` for TSR0201) and text with `{n}` arguments.
#[derive(Clone, Copy, Debug)]
pub struct SoundMessage {
    pub n: u32,
    pub text: &'static str,
}

impl SoundMessage {
    /// The rule this code belongs to (0 general, 1..=14; `parse<T>` codes count as S-14).
    pub fn rule(self) -> u8 {
        rule_of(self.n)
    }

    pub fn format(self, args: &[&str]) -> String {
        format_message(self.text, args)
    }
}

/// The rule of code number `n` (`201` is S-02; 15xx belongs to S-14).
pub fn rule_of(n: u32) -> u8 {
    match n / 100 {
        15 => 14,
        r => r as u8,
    }
}

/// Fills `{0}`, `{1}`, ... in `text`.
pub fn format_message(text: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(j) = after.find('}')
            && let Ok(k) = after[..j].parse::<usize>()
        {
            out.push_str(args.get(k).copied().unwrap_or(""));
            rest = &after[j + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Builds the diagnostic for `m` at `span` of `file`.
pub fn diagnostic(m: SoundMessage, file: FileId, span: Span, args: &[&str]) -> Diagnostic {
    Diagnostic {
        code: SOUND_CODE_BASE + m.n,
        category: Category::Error,
        message: m.format(args),
        file,
        span,
        related: Vec::new(),
    }
}

/// The sound code number of a diagnostic code (`None` for tsc and ts-r `TS` codes).
pub fn sound_number(code: u32) -> Option<u32> {
    code.checked_sub(SOUND_CODE_BASE)
}

macro_rules! messages {
    ($( $name:ident = $n:literal, $text:literal; )*) => {
        $( pub const $name: SoundMessage = SoundMessage { n: $n, text: $text }; )*
        /// Every sound-mode message, in code order.
        pub const ALL: &[SoundMessage] = &[$( $name ),*];
    };
}

messages! {
    UNSAFE_NEEDS_REASON = 1, "'@ts-r-unsafe' needs a reason: write '// @ts-r-unsafe: <why this is safe>'.";
    UNUSED_UNSAFE = 2, "Unused '@ts-r-unsafe' directive: the next line has no sound-mode error.";
    UNKNOWN_RULE = 3, "Unknown sound-mode rule '{0}' in '@ts-r-unsafe'. Rules are S-01 to S-14.";
    UNKNOWN_LEVEL = 4, "Unknown level '{0}' in '@ts-r' pragma. Use 'off', 'strict', 'sound' or 'contracts'.";
    LATE_PRAGMA = 5, "The '@ts-r' pragma must come before the first statement; this one is ignored.";
    FORCED_ISOLATED_DECLARATIONS = 10, "{0} (sound mode enables 'isolatedDeclarations').";
    FORCED_STRICT_FLAG = 11, "{0} (sound mode enables '{1}').";
    INVALID_CONFIG = 20, "Invalid sound-mode configuration: {0}.";

    NO_ANY = 101, "Type 'any' is not allowed in sound mode. Use 'unknown', a type parameter or a specific type.";
    ANY_IS_UNKNOWN = 102, "'{0}' is 'unknown' in sound mode because '{1}' declares it as 'any'. Narrow it or parse it with 'parse<T>'.";
    FUNCTION_CALL = 103, "Calling a value of type 'Function' returns 'any'. Give '{0}' a function type.";

    NARROWING_ASSERTION = 201, "Type assertion from '{0}' to '{1}' narrows the type. Use 'parse<{1}>', 'is<{1}>' or a narrowing check instead.";

    NON_NULL = 301, "Non-null assertion is not allowed in sound mode. Narrow '{0}' with a check, or use 'expect({0}, \"why\")'.";

    FORCED_UNCHECKED_INDEX = 401, "{0} (sound mode enables 'noUncheckedIndexedAccess': indexed reads can be 'undefined').";
    FORCED_EXACT_OPTIONAL = 402, "{0} (sound mode enables 'exactOptionalPropertyTypes': write '{1}?: T | undefined' to allow 'undefined').";

    INVARIANT_CONTAINER = 501, "'{0}' is not assignable to '{1}': mutable '{2}' is invariant in sound mode, so writes through '{1}' could break '{0}'. Use '{3}' if it is only read.";

    METHOD_PARAMETER = 601, "Method '{0}' is not compatible: parameter '{1}' of type '{2}' would receive '{3}'. Methods are checked like functions in sound mode.";

    GUARD_TRUE_SIDE = 701, "Type predicate '{0} is {1}' is not proved: when it returns true, '{0}' can be '{2}'.";
    GUARD_FALSE_SIDE = 702, "Type predicate '{0} is {1}' is not proved: when it returns false, '{0}' can still be '{1}'.";
    ASSERTION_NOT_PROVED = 703, "Assertion '{0}' is not proved: the function can return normally with '{1}' of type '{2}'.";

    DEFINITE_ASSIGNMENT = 801, "Definite assignment assertion on '{0}' is not allowed in sound mode. Initialise it in its declaration or the constructor.";
    THIS_BEFORE_INIT = 802, "'this' escapes before '{0}' is initialised. Assign every field first.";
    THIS_TO_OVERRIDABLE = 803, "'this' escapes from the constructor through '{0}' before subclass fields are initialised. Make it '#private', or do this after construction (a static factory).";

    NARROWING_INVALIDATED = 901, "'{0}' may have changed since it was narrowed: '{1}' on line {2} can modify it. Copy it to a 'const' before that point.";
    CAPTURED_INVALIDATED = 902, "'{0}' may have changed since it was narrowed: it is assigned in a closure, and '{1}' on line {2} can run that closure.";

    FLOATING_PROMISE = 1001, "Promise from '{0}' is not handled. Await it, return it, or attach '.catch'.";
    VOID_PROMISE = 1002, "'void' discards a promise that can reject. Attach '.catch' first: 'void {0}.catch(...)'.";
    ASYNC_VOID_CALLBACK = 1003, "Async function passed where '{0}' expects a function returning 'void'; its promise is never handled.";

    MODULE_AUGMENTATION = 1101, "Augmenting module '{0}' in a sound file changes its trusted types without a check. Move the augmentation to a '.d.ts' file.";
    SHORTHAND_MODULE = 1102, "Shorthand ambient module '{0}' makes every import 'any'. Declare its types in a '.d.ts' file.";
    AMBIENT_DECLARATION = 1103, "Ambient declaration '{0}' in a sound file asserts a value exists without checking it. Move it to a '.d.ts' file or import the real value.";

    SUPPRESSION_DIRECTIVE = 1201, "'@{0}' is not allowed in sound mode. Fix the error, or use '// @ts-r-unsafe: <reason>' for sound-mode errors.";
    NOCHECK_DIRECTIVE = 1202, "'@ts-nocheck' cannot be used in a sound file. Set the file to '// @ts-r off' instead.";
    // TSR1203 (overload implementations banned) is retired: open question 4 decided to check
    // the implementation once per overload instead (TSR1205). The number is not reused.
    GLOBAL_AUGMENTATION = 1204, "Global augmentation in a sound file changes types for the whole program without a check. Move it to a '.d.ts' file.";
    OVERLOAD_RETURN = 1205, "Overload '{0}' is not satisfied by its implementation: called through it, the body can return '{1}', which is not assignable to '{2}'.";

    UNDECLARED_THROW = 1301, "'{0}' can throw '{1}', which is not in its '@throws' tags. Catch it or add a '@throws' tag for it.";
    EXPORT_WITHOUT_THROWS = 1302, "Exported '{0}' can throw '{1}' but has no '@throws' tag. Document it with a '@throws' tag.";
    THROW_NON_ERROR = 1303, "Only Error objects can be thrown in sound mode; '{0}' is not assignable to 'Error'.";
    OVERRIDE_THROWS_MORE = 1304, "'{0}' can throw '{1}', which '{2}' (the member it overrides or implements) does not declare.";
    UNKNOWN_THROWS_TYPE = 1305, "Cannot find type '{0}' in '@throws'.";

    EXTERNAL_VALUE = 1401, "Value from external source '{0}' must be parsed before use: write 'parse<T>({1})' with the type you expect.";
    GUARD_NEEDS_TYPE_ARGUMENT = 1402, "'{0}' needs an explicit type argument in sound mode, e.g. '{0}<User>(...)'.";

    NOT_VALIDATABLE = 1501, "Type '{0}' cannot be checked at runtime: {1}.";
    GUARD_TYPE_PARAMETER = 1502, "'{0}<{1}>' needs a concrete type, but '{1}' is a type parameter. Take a 'Guard<{1}>' argument and call 'guard<User>()' at the call site.";
    GUARD_AS_VALUE = 1503, "'{0}' from '@ts-r.dev/guard' must be called with a type argument; it can't be used as a value.";
    VALIDATOR_TOO_LARGE = 1504, "The validator for '{0}' is {1} KB, over the {2} KB budget. Validate a smaller type or raise '[sound.guard] max-validator-kb'.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_in_range() {
        let mut seen = std::collections::BTreeSet::new();
        for m in ALL {
            assert!(m.n >= 1 && m.n <= 9999, "{}", m.n);
            assert!(seen.insert(m.n), "duplicate TSR{:04}", m.n);
            assert!(m.text.ends_with('.'), "TSR{:04} must end with '.'", m.n);
        }
    }

    #[test]
    fn formats_arguments() {
        assert_eq!(
            NON_NULL.format(&["x"]),
            "Non-null assertion is not allowed in sound mode. Narrow 'x' with a check, or use 'expect(x, \"why\")'."
        );
        assert_eq!(rule_of(201), 2);
        assert_eq!(rule_of(1401), 14);
        assert_eq!(rule_of(1501), 14);
        assert_eq!(rule_of(5), 0);
    }

    #[test]
    fn tsc_codes_stay_below_the_sound_range() {
        // The record asks that every code in the synced diagnosticMessages.json is < 100_000.
        // tsc 7's catalogue (with extraDiagnosticMessages.json) has CLI/LSP *message* texts at
        // 100000..=100019 ("Do not print diagnostics.", ...). They are never reported as
        // diagnostics, so they can't be confused with a TSR error in output, but they do
        // overlap TSR0000..TSR0019 numerically. This test pins that: nothing reportable
        // (error, warning, suggestion) may ever reach the sound range, and the message-only
        // overlap may not grow. See AGENTS.md ("Decisions for people").
        use ts_r_checker::diagnostics_generated::{ALL, Category as C};
        for d in ALL {
            if d.code >= SOUND_CODE_BASE {
                assert_eq!(d.category, C::Message, "tsc code {} is reportable", d.code);
                assert!(d.code <= 100_019, "tsc code {} grows the overlap", d.code);
            }
        }
    }
}
