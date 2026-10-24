//! The bundle runtime: small helpers for module wrappers and CommonJS/ESM interop. Each
//! chunk includes only the helpers it uses, under names chosen by the linker (short names
//! when minifying).
//!
//! Semantics (documented interop behaviour shared by Node, esbuild and Rollup):
//! - `__tsrCjs(fn)` returns a `require_x()` that runs `fn.call(exports, exports, module)`
//!   once and returns `module.exports`.
//! - `__tsrEsm(fn)` returns an `init_x()` that runs `fn` once.
//! - `__tsrExport(ns, getters)` defines enumerable getters (live bindings).
//! - `__tsrCopy(to, from, skip)` copies `from`'s own properties onto `to` as getters, except
//!   names `to` already has and `skip`.
//! - `__tsrToEsm(mod, nodeMode)` views CommonJS exports as an ES namespace: every property,
//!   plus `default` = `module.exports` unless the module says `__esModule` (and the importer
//!   isn't in Node's ESM mode, where `default` is always `module.exports`). `nodeMode` 2
//!   is a JSON module: `default` is always the value and, as in an ES namespace, a getter.
//! - `__tsrToCjs(ns)` views an ES namespace as CommonJS exports (`__esModule: true`); cached
//!   so `require()` returns the same object every time. A namespace with a
//!   `"module.exports"` export returns that value instead (Node's `require(esm)` rule).

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Helper {
    Cjs,
    Esm,
    Export,
    Copy,
    ToEsm,
    ToCjs,
}

impl Helper {
    pub const ALL: [Helper; 6] = [
        Helper::Cjs,
        Helper::Esm,
        Helper::Export,
        Helper::Copy,
        Helper::ToEsm,
        Helper::ToCjs,
    ];

    /// Preferred name.
    pub fn name(self) -> &'static str {
        match self {
            Helper::Cjs => "__tsrCjs",
            Helper::Esm => "__tsrEsm",
            Helper::Export => "__tsrExport",
            Helper::Copy => "__tsrCopy",
            Helper::ToEsm => "__tsrToEsm",
            Helper::ToCjs => "__tsrToCjs",
        }
    }

    fn deps(self) -> &'static [Helper] {
        match self {
            Helper::ToEsm | Helper::ToCjs => &[Helper::Copy],
            _ => &[],
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// A set of helpers with their final names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Helpers {
    pub used: [bool; 6],
    pub names: [String; 6],
}

impl Helpers {
    pub fn with_names(names: [String; 6]) -> Helpers {
        Helpers {
            used: [false; 6],
            names,
        }
    }

    pub fn name(&self, h: Helper) -> &str {
        &self.names[h.index()]
    }

    pub fn mark(&mut self, h: Helper) {
        if !self.used[h.index()] {
            self.used[h.index()] = true;
            for &d in h.deps() {
                self.mark(d);
            }
        }
    }

    pub fn merge(&mut self, other: &Helpers) {
        for h in Helper::ALL {
            if other.used[h.index()] {
                self.mark(h);
            }
        }
    }

    pub fn any(&self) -> bool {
        self.used.iter().any(|&u| u)
    }

    /// The helper code. `minify` shortens it (whitespace, syntax and local names).
    pub fn render(&self, minify: bool) -> String {
        let mut s = String::new();
        let n = |h: Helper| self.name(h).to_owned();
        let cjs = n(Helper::Cjs);
        let esm = n(Helper::Esm);
        let export = n(Helper::Export);
        let copy = n(Helper::Copy);
        let to_esm = n(Helper::ToEsm);
        let to_cjs = n(Helper::ToCjs);
        // Each helper is a self-contained `var`; locals use names that can't collide with
        // the helper names (those are reserved for the whole chunk). `__tsrCjs` and `__tsrEsm`
        // keep their state in a second parameter that callers never pass (shorter than a
        // closure `let`). The module object exists before its body runs, so a cycle sees the
        // partial exports.
        if self.used[Helper::Copy.index()] {
            let _ = write!(
                s,
                "var {copy} = (target, source, skip) => {{\n  if (source && (typeof source === \"object\" || typeof source === \"function\")) {{\n    for (const key of Object.getOwnPropertyNames(source)) {{\n      if (key !== skip && !Object.prototype.hasOwnProperty.call(target, key)) {{\n        const desc = Object.getOwnPropertyDescriptor(source, key);\n        Object.defineProperty(target, key, {{ get: () => source[key], enumerable: !desc || desc.enumerable }});\n      }}\n    }}\n  }}\n  return target;\n}};\n"
            );
        }
        if self.used[Helper::Cjs.index()] {
            let _ = writeln!(
                s,
                "var {cjs} = (body, mod) => () => (mod || body.call((mod = {{ exports: {{}} }}).exports, mod.exports, mod), mod.exports);"
            );
        }
        if self.used[Helper::Esm.index()] {
            let _ = write!(
                s,
                "var {esm} = (body, done) => () => {{\n  done || (done = 1, body());\n}};\n"
            );
        }
        if self.used[Helper::Export.index()] {
            let _ = write!(
                s,
                "var {export} = (target, getters) => {{\n  for (const key in getters) Object.defineProperty(target, key, {{ get: getters[key], enumerable: true }});\n}};\n"
            );
        }
        if self.used[Helper::ToEsm.index()] {
            let _ = write!(
                s,
                "var {to_esm} = (mod, nodeMode) => {{\n  const ns = {{}};\n  if (nodeMode || !mod || !mod.__esModule) Object.defineProperty(ns, \"default\", nodeMode > 1 ? {{ get: () => mod, enumerable: true }} : {{ value: mod, enumerable: true }});\n  return {copy}(ns, mod);\n}};\n"
            );
        }
        if self.used[Helper::ToCjs.index()] {
            let _ = write!(
                s,
                "var {to_cjs} = /* @__PURE__ */ ((cache) => (ns) => {{\n  if (\"module.exports\" in ns) return ns[\"module.exports\"];\n  let mod = cache.get(ns);\n  if (!mod) cache.set(ns, mod = {copy}(Object.defineProperty({{}}, \"__esModule\", {{ value: true }}), ns));\n  return mod;\n}})(new WeakMap());\n"
            );
        }
        if minify && !s.is_empty() {
            let out = ts_r_minify::minify(
                "runtime.js",
                &s,
                &ts_r_minify::MinifyOptions {
                    whitespace: true,
                    identifiers: true,
                    syntax: true,
                    module: false,
                },
                None,
            );
            if out.errors.is_empty() {
                let mut c = out.code;
                if !c.ends_with('\n') {
                    c.push('\n');
                }
                return c;
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_used_helpers_with_deps() {
        let mut h = Helpers::with_names(Helper::ALL.map(|h| h.name().to_owned()));
        h.mark(Helper::ToEsm);
        let text = h.render(false);
        assert!(text.contains("var __tsrCopy"));
        assert!(text.contains("var __tsrToEsm"));
        assert!(!text.contains("__tsrCjs"));
        let min = h.render(true);
        assert!(min.len() < text.len());
        assert!(min.contains("__tsrToEsm"));
    }
}
