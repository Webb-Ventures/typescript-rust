//! `--mangle-props`, `--reserve-props` and `--mangle-quoted`: the compiled patterns. The
//! names are found and rewritten by `ts_r_minify::props`; a bundle gives each name one short
//! name over every module it includes (`plan.rs`), a build without `--bundle` one per file,
//! as esbuild links each entry point on its own.

use ts_r_minify::props::{PropNames, PropOptions};

use crate::options::BuildOptions;

/// The property-mangling settings of a build.
#[derive(Debug)]
pub struct PropMatcher {
    mangle: regex_lite::Regex,
    reserve: Option<regex_lite::Regex>,
    pub quoted: bool,
    /// For preparation cache keys.
    pub key: String,
}

impl PropMatcher {
    /// `None` without `--mangle-props`; an error for a pattern that does not compile.
    pub fn from_options(o: &BuildOptions) -> Result<Option<PropMatcher>, String> {
        let Some(m) = &o.mangle_props else {
            return Ok(None);
        };
        let compile = |flag: &str, p: &str| {
            regex_lite::Regex::new(p)
                .map_err(|e| format!("Invalid regular expression for \"--{flag}=\": {e}"))
        };
        let mangle = compile("mangle-props", m)?;
        let reserve = match &o.reserve_props {
            Some(r) => Some(compile("reserve-props", r)?),
            None => None,
        };
        Ok(Some(PropMatcher {
            mangle,
            reserve,
            quoted: o.mangle_quoted,
            key: format!(
                "{m}\0{}\0{}",
                o.reserve_props.as_deref().unwrap_or(""),
                o.mangle_quoted
            ),
        }))
    }

    pub fn mangles(&self, name: &str) -> bool {
        self.mangle.is_match(name) && !self.reserve.as_ref().is_some_and(|r| r.is_match(name))
    }

    /// Runs `f` with the options for `ts_r_minify::props`.
    pub fn with_options<R>(&self, bundle: bool, f: impl FnOnce(&PropOptions<'_>) -> R) -> R {
        let mangles = |n: &str| self.mangles(n);
        f(&PropOptions {
            mangles: &mangles,
            quoted: self.quoted,
            skip_namespace_members: bundle,
        })
    }
}

/// The property names of a prepared module.
pub fn collect(m: &PropMatcher, prep: &crate::prepare::Prepared, bundle: bool) -> PropNames {
    m.with_options(bundle, |o| {
        ts_r_minify::props::collect(&prep.ast, &prep.src, Some(&prep.scopes), o)
    })
}
