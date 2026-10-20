//! `--traceResolution` output: tsc 7's log of every resolution step.
//!
//! A [`Trace`] collects the lines of the resolutions run through
//! [`crate::Resolver::resolve_module_traced`] and
//! [`crate::Resolver::resolve_type_reference_traced`], in call order, and remembers what
//! those resolutions probed: tsc notes a repeated miss ("does not exist according to earlier
//! cached lookups") and a repeated `package.json` lookup, so one `Trace` spans one program.
//! The untraced entry points pass no trace and format nothing.

use std::collections::HashSet;

/// The lines of the traced resolutions so far and what they probed.
#[derive(Debug, Clone)]
pub struct Trace {
    lines: Vec<String>,
    /// Files probed and not found (by path).
    missing: HashSet<String>,
    /// `package.json` files looked for, found or not.
    package_jsons: HashSet<String>,
    compiler_version: String,
}

impl Default for Trace {
    fn default() -> Self {
        Self::new()
    }
}

impl Trace {
    /// An empty trace.
    pub fn new() -> Self {
        let (a, b, c) = crate::TS_VERSION;
        Self {
            lines: Vec::new(),
            missing: HashSet::new(),
            package_jsons: HashSet::new(),
            compiler_version: format!("{a}.{b}.{c}"),
        }
    }

    /// The compiler version `typesVersions` messages name (default `7.0.2`; tsc's test
    /// harness prints `FakeTSVersion`). Matching still uses [`crate::TS_VERSION`].
    pub fn with_compiler_version(mut self, version: &str) -> Self {
        version.clone_into(&mut self.compiler_version);
        self
    }

    /// The lines so far.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Takes the lines so far (the probe memory stays).
    pub fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }
}

/// The optional sink every resolution step reports to.
pub(crate) struct Tracer<'a>(Option<&'a mut Trace>);

impl<'a> Tracer<'a> {
    /// No trace: nothing is formatted.
    pub(crate) fn off() -> Self {
        Tracer(None)
    }

    pub(crate) fn on(t: &'a mut Trace) -> Self {
        Tracer(Some(t))
    }

    #[inline]
    pub(crate) fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Adds the line `f` builds (only called when tracing).
    #[inline]
    pub(crate) fn line(&mut self, f: impl FnOnce() -> String) {
        if let Some(t) = &mut self.0 {
            t.lines.push(f());
        }
    }

    /// A probe of a candidate file.
    #[inline]
    pub(crate) fn file(&mut self, path: &str, exists: bool) {
        let Some(t) = &mut self.0 else { return };
        let line = if exists {
            format!("File '{path}' exists - use it as a name resolution result.")
        } else if t.missing.contains(path) {
            format!("File '{path}' does not exist according to earlier cached lookups.")
        } else {
            t.missing.insert(path.to_owned());
            format!("File '{path}' does not exist.")
        };
        t.lines.push(line);
    }

    /// A lookup of `file` (a `package.json`).
    #[inline]
    pub(crate) fn package_json(&mut self, file: &str, exists: bool) {
        let Some(t) = &mut self.0 else { return };
        let line = if !t.package_jsons.insert(file.to_owned()) || t.missing.contains(file) {
            if exists {
                format!("File '{file}' exists according to earlier cached lookups.")
            } else {
                format!("File '{file}' does not exist according to earlier cached lookups.")
            }
        } else if exists {
            format!("Found 'package.json' at '{file}'.")
        } else {
            t.missing.insert(file.to_owned());
            format!("File '{file}' does not exist.")
        };
        t.lines.push(line);
    }

    /// A directory that does not exist.
    #[inline]
    pub(crate) fn no_directory(&mut self, dir: &str) {
        self.line(|| format!("Directory '{dir}' does not exist, skipping all lookups in it."));
    }

    pub(crate) fn compiler_version(&self) -> &str {
        self.0.as_ref().map_or("", |t| t.compiler_version.as_str())
    }
}

/// `tr!(tracer, "format", args)`: adds a line when tracing.
macro_rules! tr {
    ($t:expr, $($arg:tt)*) => {
        $t.line(|| format!($($arg)*))
    };
}
pub(crate) use tr;
