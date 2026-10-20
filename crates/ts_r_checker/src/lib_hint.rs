//! "Do you need to change your target library?" for a missing lib member (TS2550).
//!
//! Observed from tsc 7.0.2: a property missing on a global lib type is TS2550 in place of
//! TS2339 when the member belongs to a later lib, naming that lib's year
//! (`[].at` without `es2022` suggests `es2022`). The container is the apparent type's
//! interface (a type parameter through its constraint, a primitive through its wrapper:
//! `"".padStart` is `String`), or a lib namespace (`typeof Intl`). Tuples, unions,
//! intersections, subclasses and element accesses get no hint. Only some later members
//! are hinted (not `ReadonlyArray`'s, the iterator helpers or typed arrays' `entries`);
//! the table holds the pairs that probing every member the bundled lib files add after
//! es5 showed to be hinted, each with the year tsc named (always the year of the first lib
//! file declaring the member).

use ts_r_types::{ObjectKind, SymId, TypeId};

use crate::checker::Ck;

impl Ck<'_, '_> {
    /// The lib year to suggest for property `name` missing on `t` (TS2550), if any.
    pub(crate) fn target_library_hint(&mut self, t: TypeId, name: &str) -> Option<&'static str> {
        let a = self.apparent_type(t);
        let s = self.store();
        let (sym, namespace) = match s.object_of(a)?.kind {
            ObjectKind::Reference { target, .. } => match s.object_of(target)?.kind {
                ObjectKind::Declared { sym, .. } => (sym, false),
                _ => return None,
            },
            ObjectKind::Declared { sym, .. } => (sym, false),
            ObjectKind::Anonymous { decl, kind, .. }
                if decl.0 == u32::MAX && kind == crate::declared::anon::NAMESPACE =>
            {
                (SymId(decl.1), true)
            }
            _ => return None,
        };
        if !self
            .c
            .first_decl(sym)
            .is_some_and(|d| self.c.file(d.file).is_lib)
        {
            return None;
        }
        let container = self.c.sym_name(sym);
        if namespace {
            return if container == "Intl" {
                lib_member("typeof Intl", name)
            } else {
                None
            };
        }
        lib_member(container, name)
    }
}

/// The observed TS2550 table: container (interface name, `typeof` for a namespace) and
/// member to the lib year tsc names.
fn lib_member(container: &str, member: &str) -> Option<&'static str> {
    Some(match container {
        "Array" => match member {
            "copyWithin" | "entries" | "fill" | "find" | "findIndex" | "keys" | "values" => {
                "es2015"
            }
            "includes" => "es2016",
            "flat" | "flatMap" => "es2019",
            "at" => "es2022",
            "findLast" | "findLastIndex" | "toReversed" | "toSorted" | "toSpliced" | "with" => {
                "es2023"
            }
            _ => return None,
        },
        "ArrayBuffer" => match member {
            "detached"
            | "maxByteLength"
            | "resizable"
            | "resize"
            | "transfer"
            | "transferToFixedLength" => "es2024",
            _ => return None,
        },
        "ArrayConstructor" => match member {
            "from" | "of" => "es2015",
            "fromAsync" => "esnext",
            _ => return None,
        },
        "Atomics" => match member {
            "waitAsync" => "es2024",
            _ => return None,
        },
        "BigInt64Array" | "BigUint64Array" | "Float32Array" | "Float64Array" | "Int16Array"
        | "Int32Array" | "Int8Array" | "Uint16Array" | "Uint32Array" | "Uint8Array"
        | "Uint8ClampedArray" => match member {
            "at" => "es2022",
            "findLast" | "findLastIndex" | "toReversed" | "toSorted" | "with" => "es2023",
            _ => return None,
        },
        "DataView" => match member {
            "getBigInt64" | "getBigUint64" | "setBigInt64" | "setBigUint64" => "es2020",
            "getFloat16" | "setFloat16" => "es2025",
            _ => return None,
        },
        "Date" => match member {
            "toTemporalInstant" => "esnext",
            _ => return None,
        },
        "DateTimeFormat" => match member {
            "formatToParts" => "es2017",
            _ => return None,
        },
        "Error" => match member {
            "cause" => "es2022",
            _ => return None,
        },
        "ErrorConstructor" => match member {
            "isError" => "esnext",
            _ => return None,
        },
        "Map" | "WeakMap" => match member {
            "getOrInsert" | "getOrInsertComputed" => "esnext",
            _ => return None,
        },
        "MapConstructor" => match member {
            "groupBy" => "es2024",
            _ => return None,
        },
        "Math" => match member {
            "acosh" | "asinh" | "atanh" | "cbrt" | "clz32" | "cosh" | "expm1" | "fround"
            | "hypot" | "imul" | "log10" | "log1p" | "log2" | "sign" | "sinh" | "tanh"
            | "trunc" => "es2015",
            "f16round" => "es2025",
            _ => return None,
        },
        "NumberConstructor" => match member {
            "isFinite" | "isInteger" | "isNaN" | "isSafeInteger" | "parseFloat" | "parseInt" => {
                "es2015"
            }
            _ => return None,
        },
        "NumberFormat" => match member {
            "formatToParts" => "es2018",
            _ => return None,
        },
        "ObjectConstructor" => match member {
            "assign" | "getOwnPropertySymbols" | "is" | "setPrototypeOf" => "es2015",
            "entries" | "getOwnPropertyDescriptors" | "values" => "es2017",
            "fromEntries" => "es2019",
            "hasOwn" => "es2022",
            "groupBy" => "es2024",
            _ => return None,
        },
        "Promise" => match member {
            "finally" => "es2018",
            _ => return None,
        },
        "PromiseConstructor" => match member {
            "allSettled" => "es2020",
            "any" => "es2021",
            "withResolvers" => "es2024",
            "try" => "es2025",
            _ => return None,
        },
        "RegExp" => match member {
            "flags" | "sticky" | "unicode" => "es2015",
            "dotAll" => "es2018",
            "unicodeSets" => "es2024",
            _ => return None,
        },
        "RegExpConstructor" => match member {
            "escape" => "es2025",
            _ => return None,
        },
        "RegExpExecArray" | "RegExpMatchArray" => match member {
            "groups" => "es2018",
            _ => return None,
        },
        "Set" => match member {
            "difference"
            | "intersection"
            | "isDisjointFrom"
            | "isSubsetOf"
            | "isSupersetOf"
            | "symmetricDifference"
            | "union" => "es2025",
            _ => return None,
        },
        "SharedArrayBuffer" => match member {
            "grow" | "growable" | "maxByteLength" => "es2024",
            _ => return None,
        },
        "String" => match member {
            "anchor" | "big" | "blink" | "bold" | "codePointAt" | "endsWith" | "fixed"
            | "fontcolor" | "fontsize" | "includes" | "italics" | "link" | "normalize"
            | "repeat" | "small" | "startsWith" | "strike" | "sub" | "sup" => "es2015",
            "padEnd" | "padStart" => "es2017",
            "trimEnd" | "trimLeft" | "trimRight" | "trimStart" => "es2019",
            "matchAll" => "es2020",
            "replaceAll" => "es2021",
            "at" => "es2022",
            "isWellFormed" | "toWellFormed" => "es2024",
            _ => return None,
        },
        "StringConstructor" => match member {
            "fromCodePoint" | "raw" => "es2015",
            _ => return None,
        },
        "Symbol" => match member {
            "description" => "es2019",
            _ => return None,
        },
        "SymbolConstructor" => match member {
            "matchAll" => "es2020",
            "asyncDispose" | "dispose" | "metadata" => "esnext",
            _ => return None,
        },
        "Uint8ArrayConstructor" => match member {
            "fromBase64" | "fromHex" => "esnext",
            _ => return None,
        },
        "typeof Intl" => match member {
            "PluralRules" => "es2018",
            "DisplayNames" | "Locale" | "RelativeTimeFormat" => "es2020",
            "ListFormat" => "es2021",
            "Segmenter" => "es2022",
            "DurationFormat" => "es2025",
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::lib_member;

    #[test]
    fn table_lookups() {
        assert_eq!(lib_member("Array", "at"), Some("es2022"));
        assert_eq!(lib_member("Uint8Array", "findLast"), Some("es2023"));
        assert_eq!(lib_member("Uint8Array", "entries"), None);
        assert_eq!(lib_member("ReadonlyArray", "at"), None);
        assert_eq!(lib_member("typeof Intl", "PluralRules"), Some("es2018"));
        assert_eq!(lib_member("String", "trimEnd"), Some("es2019"));
    }
}
