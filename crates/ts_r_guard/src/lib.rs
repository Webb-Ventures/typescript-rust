//! `parse<T>` runtime validators (design record 0007, "`parse<T>` runtime validators").
//!
//! [`Generator`] evaluates type annotations of a loaded program on declarations only (aliases,
//! interfaces, enums, imports and re-exports; never the checker's types) and emits, per type,
//! a compiled predicate and a descriptor. [`RUNTIME`] holds the shared helpers and the
//! descriptor walker that builds a `ParseError`. [`Generator::module`] wraps everything in one
//! ES module. [`expand`] rewrites `parse<T>` call sites of a file (inline validators for
//! `ts-r-run`, `ts-r-dev` and unbundled output); [`vfs::GuardFs`] serves the bundler one
//! virtual module per validated declaration.

mod emit;
mod eval;
pub mod expand;
mod rt;
pub mod vfs;

pub use expand::{ExpandOptions, Expanded, Expander, GuardError, Layout};

use ts_r_ast::NodeId;
use ts_r_checker::ProgramFiles;

pub use eval::Unsupported;

/// The shared runtime (`$parse`, `$has`, `$isArr`, the descriptor walker, `ParseError`).
pub const RUNTIME: &str = include_str!("runtime.js");

/// `[sound.guard] explain`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Explain {
    /// Up to 20 issues.
    #[default]
    Full,
    /// The first issue only.
    First,
    /// No descriptors: `Invalid User`.
    None,
}

/// `[sound.guard]` (record 0007, "Configuration"). The tools read it from `ts-r.toml`,
/// `tsrconfig.json` or the tsconfig's `"ts-r"` key through
/// `ts_r_sound::config::guard_options`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GuardOptions {
    /// `max-depth`: nesting of named types before a `ParseError`.
    pub max_depth: u32,
    pub explain: Explain,
    /// `max-validator-kb`: a validator (predicate and descriptor of the type and every
    /// declaration it reaches, as generated, before minification) over this many KB is
    /// TSR1504. 0 turns the check off.
    pub max_validator_kb: u32,
    /// `refinements`: registered predicates for branded and refined types, in the order
    /// configured (record 0007, "Branded and refined types").
    pub refinements: Vec<Refinement>,
}

impl Default for GuardOptions {
    fn default() -> Self {
        GuardOptions {
            max_depth: 512,
            explain: Explain::Full,
            max_validator_kb: 8,
            refinements: Vec::new(),
        }
    }
}

/// One `[sound.guard] refinements` entry: `Email = "./validators#isEmail"`. When a validated
/// type reaches a type alias or interface named `name`, the validator checks its base type
/// (the alias without its brand members) and then calls `export` of `module`, a
/// `(v: unknown) => v is T` predicate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Refinement {
    /// The type's declared name (`Email`).
    pub name: String,
    /// A package specifier (`my-validators`), or an absolute path for a relative specifier
    /// in the configuration (resolved against the configuration file's directory, the
    /// extension kept as written).
    pub module: String,
    /// The exported predicate (`isEmail`, or `default`).
    pub export: String,
}

impl Refinement {
    /// Whether `module` is a path (a relative specifier in the configuration) rather than
    /// a package specifier.
    pub fn is_path(&self) -> bool {
        ts_r_config::path::is_rooted(&self.module)
    }
}

/// One validated type: `$is_<entry>(v, d)` and `$d_<entry>()` in the generated code.
#[derive(Clone, Debug)]
pub struct Entry {
    pub entry: String,
    /// The type as written (`User`, `Page<User>`), for messages.
    pub type_name: String,
}

/// Collects validated types of one output (a file or a chunk) and emits their code.
#[derive(Debug)]
pub struct Generator<'p> {
    eval: eval::Eval<'p>,
    opts: GuardOptions,
    entries: Vec<(Entry, eval::G)>,
}

impl<'p> Generator<'p> {
    pub fn new(program: &'p ProgramFiles, opts: GuardOptions) -> Generator<'p> {
        let mut eval = eval::Eval::new(program);
        eval.refined = opts.refinements.iter().map(|r| r.name.clone()).collect();
        Generator {
            eval,
            opts,
            entries: Vec::new(),
        }
    }

    /// An emitter over the declarations so far, with class and refinement bindings: a class
    /// declared in `local_file` (outside `declare module`) is named by its own binding there,
    /// every other class by its imported `$c_<Name>`.
    pub(crate) fn emitter(&self, local_file: Option<usize>) -> emit::Emitter<'_> {
        let (classes, refines) = self.bindings(local_file);
        emit::Emitter::new(&self.eval.decls).with_bindings(classes, refines)
    }

    /// The bindings [`Generator::emitter`] uses.
    #[allow(clippy::type_complexity)]
    pub(crate) fn bindings(
        &self,
        local_file: Option<usize>,
    ) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let classes = self
            .eval
            .classes
            .iter()
            .map(|c| {
                let local = Some(c.file) == local_file && c.module.is_none() && c.name != "default";
                let binding = if local {
                    c.name.clone()
                } else {
                    c.binding.clone()
                };
                (binding, c.name.clone())
            })
            .collect();
        let refines = self
            .opts
            .refinements
            .iter()
            .map(|r| (refine_binding(&r.name), r.name.clone()))
            .collect();
        (classes, refines)
    }

    /// Adds the type annotation `ty` of file `file` (a `parse<T>` type argument).
    pub fn add(&mut self, file: usize, ty: NodeId) -> Result<Entry, Unsupported> {
        let program = self.eval.program();
        let written = program
            .files
            .get(file)
            .map(|f| {
                let sp = f.ast.span(ty);
                f.text
                    .get(sp.start as usize..sp.end as usize)
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let mark = self.eval.mark();
        match self.eval.top(file, ty) {
            Ok(g) => {
                let e = Entry {
                    entry: format!("e{}", self.entries.len()),
                    type_name: written,
                };
                self.entries.push((e.clone(), g));
                Ok(e)
            }
            Err(reason) => {
                self.eval.rollback(mark);
                Err(Unsupported {
                    ty: written,
                    reason,
                })
            }
        }
    }

    /// The code for every added type: declaration functions and the entries (`$is_e0`,
    /// `$d_e0`, ...). Needs the runtime in scope (see [`Generator::module`]).
    pub fn code(&self) -> String {
        let mut em = self.emitter(None);
        let mut out = String::new();
        em.decl_functions(&mut out, self.opts.max_depth);
        for (e, g) in &self.entries {
            let n = &e.entry;
            em.entry_functions(g, &format!("$is_{n}"), &format!("$d_{n}"), &mut out);
        }
        out
    }

    /// The expression `parse<T>(value)` becomes for an entry.
    pub fn parse_call(&self, e: &Entry, value: &str) -> String {
        let n = &e.entry;
        parse_expr(
            self.opts.explain,
            &format!("$is_{n}"),
            &format!("$d_{n}"),
            &e.type_name,
            value,
        )
    }

    /// One ES module: the runtime parts the code reaches, the code, and per entry
    /// `export const <entry> = { is, parse, safeParse, assert }`, plus `ParseError`.
    pub fn module(&self) -> String {
        let mut code = self.code();
        for (e, _) in &self.entries {
            let n = &e.entry;
            code.push_str(&format!(
                "export const {n} = {{\n  is: (v) => $is_{n}(v, 0),\n  parse: (v) => {p},\n  safeParse(v) {{ return $safe(this.parse, v, $is_{n}); }},\n  assert(v) {{ this.parse(v); }},\n}};\n",
                p = self.parse_call(e, "v"),
            ));
        }
        code.push_str("export const ParseError = $ParseError();\n");
        // Classes and refinement predicates, imported by absolute path or package name.
        let gs: Vec<&eval::G> = self
            .entries
            .iter()
            .map(|(_, g)| g)
            .chain(self.eval.decls.iter().filter_map(|d| d.body.as_ref()))
            .collect();
        let mut out = expand::ext_imports(self, &gs, None, None);
        out.push_str(&rt::runtime_for(&code, self.opts.max_depth));
        out.push_str(&code);
        out
    }
}

/// The binding of the refinement predicate for type `name` (`$r_Email`).
pub(crate) fn refine_binding(name: &str) -> String {
    let base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("$r_{base}")
}

/// `$parse(value, is, d, "T")` (at most 20 issues), `.., 1)` for `explain = "first"`, or
/// `$parse0(value, is, "T")` (no descriptors, no walker) for `explain = "none"`.
pub(crate) fn parse_expr(
    explain: Explain,
    is: &str,
    d: &str,
    type_name: &str,
    value: &str,
) -> String {
    let t = emit::js_str(type_name);
    match explain {
        Explain::Full => format!("$parse({value}, {is}, {d}, {t})"),
        Explain::First => format!("$parse({value}, {is}, {d}, {t}, 1)"),
        Explain::None => format!("$parse0({value}, {is}, {t})"),
    }
}
