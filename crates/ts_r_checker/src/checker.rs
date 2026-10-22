//! The shared checker state ([`Checker`]) and the per-thread worker ([`Ck`]).
//!
//! `Checker` is `Sync`: everything computed lazily (symbol types, resolved members,
//! signatures' return types) is published into write-once slots; whoever computes first
//! wins and computations are deterministic, so a race only wastes work. A `Ck` holds the
//! scratch state of one thread: the file being checked, its diagnostics, node type caches,
//! the circularity stack and flow caches.

use std::sync::{Arc, OnceLock};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, Node, NodeId, Span};
use ts_r_binder::BoundFile;
use ts_r_binder::program::{Program, ProgramFile};
use ts_r_config::{CompilerOptions, JsxEmit, ModuleKind, ScriptTarget, StrictFlag};
use ts_r_diagnostics::{Diagnostic, FileId};
use ts_r_types::{ChunkVec, SigId, SymId, TypeId, TypeStore};

use crate::diag::{Chain, dg, msg};
use crate::layers::{Layer, Shadows};
use crate::program::{ProgramFiles, Resolution, SourceFile};
use crate::symbols::{SymLinks, SymbolTable};

/// A node of a particular file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Loc {
    pub file: FileId,
    pub node: NodeId,
}

impl Loc {
    pub fn new(file: FileId, node: NodeId) -> Loc {
        Loc { file, node }
    }

    pub fn pack(self) -> (u32, u32) {
        (self.file.0, self.node.raw())
    }

    pub fn unpack(p: (u32, u32)) -> Option<Loc> {
        Some(Loc {
            file: FileId(p.0),
            node: NodeId::from_raw(p.1)?,
        })
    }
}

/// Options that change checking.
#[derive(Clone, Debug)]
pub struct CheckOptions {
    pub strict_null_checks: bool,
    pub no_implicit_any: bool,
    pub strict_function_types: bool,
    pub strict_property_initialization: bool,
    pub strict_bind_call_apply: bool,
    /// `strictBuiltinIteratorReturn`: the lib's `BuiltinIteratorReturn` is `undefined`
    /// (else `any`).
    pub strict_builtin_iterator_return: bool,
    pub no_implicit_this: bool,
    pub use_unknown_in_catch_variables: bool,
    pub exact_optional_property_types: bool,
    pub no_unchecked_indexed_access: bool,
    pub no_implicit_returns: bool,
    pub no_implicit_override: bool,
    pub no_property_access_from_index_signature: bool,
    pub no_fallthrough_cases_in_switch: bool,
    /// Types print untruncated (cycles as `any`, not `...`).
    pub no_error_truncation: bool,
    pub no_unused_locals: bool,
    pub no_unused_parameters: bool,
    pub allow_unreachable_code: Option<bool>,
    pub skip_lib_check: bool,
    pub isolated_modules: bool,
    pub verbatim_module_syntax: bool,
    pub es_module_interop: bool,
    pub allow_synthetic_default_imports: bool,
    pub resolve_json_module: bool,
    pub target: ScriptTarget,
    /// The effective `module`.
    pub module: ModuleKind,
    pub no_lib: bool,
    pub allow_js: bool,
    pub check_js: bool,
    /// `allowImportingTsExtensions` (or `rewriteRelativeImportExtensions`).
    pub allow_importing_ts_extensions: bool,
    pub use_define_for_class_fields: bool,
    pub experimental_decorators: bool,
    /// `jsx` is set (any mode).
    pub jsx_set: bool,
    /// `jsx` is `react-jsx` or `react-jsxdev` (the JSX namespace may come from the
    /// `jsxImportSource` module instead of the global scope).
    pub jsx_automatic: bool,
    /// Root names of the JSX factories (`jsxFactory`, `jsxFragmentFactory`,
    /// `reactNamespace`; default `React`): JSX uses them implicitly.
    pub jsx_factory_roots: Vec<String>,
    /// `jsx`.
    pub jsx: Option<JsxEmit>,
    /// `jsxImportSource` (default `react`).
    pub jsx_import_source: String,
    /// A valid `jsxFactory` is set (an unresolved factory is reported as a name).
    pub jsx_factory_custom: bool,
    /// `jsxFragmentFactory` is set.
    pub jsx_fragment_factory_set: bool,
    /// `importHelpers`.
    pub import_helpers: bool,
    /// `noEmit`.
    pub no_emit: bool,
    /// The effective `moduleResolution` is `node16` or `nodenext`.
    pub node_resolution: bool,
    /// `jsx` is `preserve` (`.tsx` files emit `.jsx`).
    pub jsx_preserve: bool,
}

impl CheckOptions {
    pub fn from_compiler_options(o: &CompilerOptions) -> CheckOptions {
        let e = o.effective();
        let flag = |v: Option<bool>| v.unwrap_or(false);
        CheckOptions {
            strict_null_checks: e.strict_flag(StrictFlag::StrictNullChecks),
            no_implicit_any: e.strict_flag(StrictFlag::NoImplicitAny),
            strict_function_types: e.strict_flag(StrictFlag::StrictFunctionTypes),
            strict_property_initialization: e.strict_flag(StrictFlag::StrictPropertyInitialization)
                && e.strict_flag(StrictFlag::StrictNullChecks),
            strict_bind_call_apply: e.strict_flag(StrictFlag::StrictBindCallApply),
            strict_builtin_iterator_return: e.strict_flag(StrictFlag::StrictBuiltinIteratorReturn),
            no_implicit_this: e.strict_flag(StrictFlag::NoImplicitThis),
            use_unknown_in_catch_variables: e.strict_flag(StrictFlag::UseUnknownInCatchVariables),
            exact_optional_property_types: flag(o.exact_optional_property_types()),
            no_unchecked_indexed_access: flag(o.no_unchecked_indexed_access()),
            no_implicit_returns: flag(o.no_implicit_returns()),
            no_implicit_override: flag(o.no_implicit_override()),
            no_property_access_from_index_signature: flag(
                o.no_property_access_from_index_signature(),
            ),
            no_fallthrough_cases_in_switch: flag(o.no_fallthrough_cases_in_switch()),
            no_error_truncation: flag(o.no_error_truncation()),
            no_unused_locals: flag(o.no_unused_locals()),
            no_unused_parameters: flag(o.no_unused_parameters()),
            allow_unreachable_code: o.allow_unreachable_code(),
            skip_lib_check: flag(o.skip_lib_check()),
            isolated_modules: e.isolated_modules(),
            verbatim_module_syntax: flag(o.verbatim_module_syntax()),
            es_module_interop: e.es_module_interop(),
            allow_synthetic_default_imports: e.allow_synthetic_default_imports(),
            resolve_json_module: e.resolve_json_module(),
            target: e.target(),
            module: e.module(),
            no_lib: flag(o.no_lib()),
            allow_js: e.allow_js(),
            jsx_set: o.jsx().is_some(),
            jsx_automatic: matches!(o.jsx(), Some(JsxEmit::ReactJsx | JsxEmit::ReactJsxDev)),
            check_js: flag(o.check_js()),
            allow_importing_ts_extensions: e.allow_importing_ts_extensions(),
            use_define_for_class_fields: e.use_define_for_class_fields(),
            experimental_decorators: flag(o.experimental_decorators()),
            jsx_factory_roots: jsx_factory_roots(o),
            jsx: o.jsx(),
            jsx_import_source: e.jsx_import_source().to_owned(),
            jsx_factory_custom: o.jsx_factory().is_some_and(is_qualified_name),
            jsx_fragment_factory_set: o.jsx_fragment_factory().is_some(),
            import_helpers: flag(o.import_helpers()),
            no_emit: e.no_emit(),
            node_resolution: matches!(
                e.resolution_algorithm(),
                ts_r_config::ModuleResolution::Node16 | ts_r_config::ModuleResolution::NodeNext
            ),
            jsx_preserve: matches!(o.jsx(), Some(JsxEmit::Preserve)),
        }
    }
}

fn jsx_factory_roots(o: &CompilerOptions) -> Vec<String> {
    let root = |s: &str| s.split('.').next().unwrap_or("").trim().to_owned();
    let mut out = vec![match (
        o.jsx_factory().filter(|f| is_qualified_name(f)),
        o.react_namespace(),
    ) {
        (Some(f), _) => root(f),
        (None, Some(ns)) => root(ns),
        (None, None) => "React".to_owned(),
    }];
    if let Some(f) = o.jsx_fragment_factory().filter(|f| is_qualified_name(f)) {
        out.push(root(f));
    }
    out
}

/// `a` or `a.b.c` with identifier parts (a valid `jsxFactory`; invalid ones are TS5067 and
/// fall back to `React`).
pub(crate) fn is_qualified_name(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| {
            let mut cs = p.chars();
            cs.next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
}

/// Global types looked up by name in the lib files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum GlobalType {
    Array,
    ReadonlyArray,
    Object,
    Function,
    CallableFunction,
    NewableFunction,
    String,
    Number,
    Boolean,
    Symbol,
    BigInt,
    RegExp,
    IArguments,
    Promise,
    PromiseLike,
    Iterable,
    Iterator,
    IterableIterator,
    AsyncIterable,
    AsyncIterator,
    AsyncIterableIterator,
    TemplateStringsArray,
    Generator,
    AsyncGenerator,
    Error,
    IteratorObject,
    ArrayIterator,
    Awaited,
    Disposable,
    AsyncDisposable,
}

impl GlobalType {
    pub const COUNT: usize = 30;

    pub fn name(self) -> &'static str {
        match self {
            GlobalType::Array => "Array",
            GlobalType::ReadonlyArray => "ReadonlyArray",
            GlobalType::Object => "Object",
            GlobalType::Function => "Function",
            GlobalType::CallableFunction => "CallableFunction",
            GlobalType::NewableFunction => "NewableFunction",
            GlobalType::String => "String",
            GlobalType::Number => "Number",
            GlobalType::Boolean => "Boolean",
            GlobalType::Symbol => "Symbol",
            GlobalType::BigInt => "BigInt",
            GlobalType::RegExp => "RegExp",
            GlobalType::IArguments => "IArguments",
            GlobalType::Promise => "Promise",
            GlobalType::PromiseLike => "PromiseLike",
            GlobalType::Iterable => "Iterable",
            GlobalType::Iterator => "Iterator",
            GlobalType::IterableIterator => "IterableIterator",
            GlobalType::AsyncIterable => "AsyncIterable",
            GlobalType::AsyncIterator => "AsyncIterator",
            GlobalType::AsyncIterableIterator => "AsyncIterableIterator",
            GlobalType::TemplateStringsArray => "TemplateStringsArray",
            GlobalType::Generator => "Generator",
            GlobalType::AsyncGenerator => "AsyncGenerator",
            GlobalType::Error => "Error",
            GlobalType::IteratorObject => "IteratorObject",
            GlobalType::ArrayIterator => "ArrayIterator",
            GlobalType::Awaited => "Awaited",
            GlobalType::Disposable => "Disposable",
            GlobalType::AsyncDisposable => "AsyncDisposable",
        }
    }
}

/// Shared state of a check.
pub struct Checker<'a> {
    pub files: &'a [Arc<SourceFile>],
    pub link: Program<'a>,
    /// The top level's type store (it sees every lower level's types).
    pub store: Arc<TypeStore>,
    pub opts: CheckOptions,
    pub resolutions: &'a FxHashMap<(String, String), Option<Resolution>>,
    /// See [`crate::program::ProgramFiles::mode_resolutions`].
    pub mode_resolutions: &'a crate::program::ModeResolutions,
    /// Program levels, bottom first; the last is this checker's own (see `layers.rs`). A
    /// checker made by [`Checker::new`] has one level holding every file.
    pub(crate) layers: Vec<Arc<Layer>>,
    globals: [OnceLock<Option<TypeId>>; GlobalType::COUNT],
    /// Lazily created helper types (shared).
    pub(crate) cached: CachedTypes,
    /// The global `Array` and `ReadonlyArray` type symbols.
    pub(crate) array_syms: [Option<SymId>; 2],
    /// Per file: specifier node → index of its first module request (built on first use).
    request_index: Box<[OnceLock<FxHashMap<NodeId, u32>>]>,
}

/// Helper types created once per checker.
#[derive(Default)]
pub(crate) struct CachedTypes {
    pub string_number_symbol: OnceLock<TypeId>,
    /// Whether any file declares a `const` type parameter.
    pub has_const_type_params: OnceLock<bool>,
    /// Declaration emit: per symbol, the modules that re-export it and the export name, in
    /// program order (`typenode::Ck::reexporters`).
    pub reexporters:
        OnceLock<rustc_hash::FxHashMap<ts_r_types::SymId, Vec<(ts_r_types::SymId, String)>>>,
    /// Measured variances of generic classes and interfaces, by declared type.
    pub variances: std::sync::Mutex<
        rustc_hash::FxHashMap<crate::variance::VarianceKey, Arc<crate::variance::Variances>>,
    >,
}

impl std::fmt::Debug for Checker<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checker")
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

/// Why levels cannot be used for a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerError {
    /// The program's first files are not the parent level's files (same `Arc`s, in order).
    FilesDiffer,
    /// A merged declaration has parts in two levels.
    CrossLevelMerge,
    /// The parent is already at the highest store level.
    TooManyLevels,
}

impl<'a> Checker<'a> {
    /// Links the program's files and prepares the shared state.
    pub fn new(program: &'a ProgramFiles) -> Checker<'a> {
        match Checker::layered(program, None) {
            Ok(c) => c,
            Err(_) => unreachable!("a single-level checker has no level errors"),
        }
    }

    /// A checker whose own level holds the program's files after `parent`'s (which must be
    /// the program's first files, the same `Arc`s in the same order). It sees everything the
    /// parent levels computed and adds to them what involves only their entities; its own
    /// level can be kept with [`Checker::top_layer`] and built upon later.
    pub fn layered(
        program: &'a ProgramFiles,
        parent: Option<&Arc<Layer>>,
    ) -> Result<Checker<'a>, LayerError> {
        let files = &program.files[..];
        let file_start = parent.map_or(0, |p| p.file_end());
        if let Some(p) = parent {
            let below = p.all_files();
            if below.len() > files.len() || below.iter().zip(files).any(|(a, b)| !Arc::ptr_eq(a, b))
            {
                return Err(LayerError::FilesDiffer);
            }
        }
        let pfiles: Vec<ProgramFile<'a>> = files
            .iter()
            .map(|f| ProgramFile {
                path: f.path.as_str(),
                ast: &f.ast,
                src: f.text.as_str(),
                bound: &f.bound,
            })
            .collect();
        let resolutions = &program.resolutions;
        let mode_resolutions = &program.mode_resolutions;
        let module = program.options.effective().module();
        // Formats by path, only needed to look up `mode_resolutions`.
        let formats: FxHashMap<&str, bool> = if mode_resolutions.is_empty() {
            FxHashMap::default()
        } else {
            files
                .iter()
                .map(|f| (f.path.as_str(), f.package_esm))
                .collect()
        };
        let link = Program::link(pfiles, |spec, containing, req| {
            formats
                .get(containing)
                .map(|&esm| crate::program::request_mode(module, containing, esm, req))
                .and_then(|m| mode_resolutions.get(&(containing.to_owned(), spec.to_owned(), m)))
                .or_else(|| resolutions.get(&(containing.to_owned(), spec.to_owned())))
                .and_then(|r| r.as_ref())
                .filter(|r| r.included)
                .map(|r| r.resolved.clone())
        });
        let level = parent.map_or(0, |p| p.level + 1);
        let store = match parent {
            None => TypeStore::new(),
            Some(p) => TypeStore::child(&p.store, file_start).ok_or(LayerError::TooManyLevels)?,
        };
        let syms = SymbolTable::build(files, &link, level, file_start, files.len() as u32);
        if syms.crosses_levels {
            return Err(LayerError::CrossLevelMerge);
        }
        let mut links = Vec::with_capacity(syms.merged_end() as usize);
        links.resize_with(syms.merged_end() as usize, SymLinks::default);
        let store = Arc::new(store);
        let top = Arc::new(Layer {
            level,
            files: files[file_start as usize..].to_vec(),
            file_start,
            syms,
            links: links.into_boxed_slice(),
            synth: ChunkVec::new(),
            shadows: Shadows::default(),
            instantiation_query_names: Default::default(),
            store: store.clone(),
            parent: parent.cloned(),
        });
        let mut layers: Vec<Arc<Layer>> = Vec::with_capacity(level as usize + 1);
        let mut cur = parent.cloned();
        while let Some(l) = cur {
            cur = l.parent.clone();
            layers.push(l);
        }
        layers.reverse();
        layers.push(top);
        let mut c = Checker {
            files,
            link,
            store,
            opts: CheckOptions::from_compiler_options(&program.options),
            resolutions,
            mode_resolutions: &program.mode_resolutions,
            layers,
            globals: std::array::from_fn(|_| OnceLock::new()),
            cached: CachedTypes::default(),
            array_syms: [None, None],
            request_index: files.iter().map(|_| OnceLock::new()).collect(),
        };
        c.array_syms = [
            c.global_symbol("Array", ts_r_binder::meaning::TYPE),
            c.global_symbol("ReadonlyArray", ts_r_binder::meaning::TYPE),
        ];
        Ok(c)
    }

    /// The index in `file`'s module requests of the request whose specifier is `node`.
    pub(crate) fn request_of(&self, file: FileId, node: NodeId) -> Option<u32> {
        self.request_index[file.0 as usize]
            .get_or_init(|| {
                let mut m = FxHashMap::default();
                for (i, r) in self.bound(file).module_requests.iter().enumerate() {
                    m.entry(r.node).or_insert(i as u32);
                }
                m
            })
            .get(&node)
            .copied()
    }

    /// This checker's own level: keep it to build later checkers on it.
    pub fn top_layer(&self) -> &Arc<Layer> {
        &self.layers[self.layers.len() - 1]
    }

    #[inline]
    pub fn file(&self, f: FileId) -> &'a SourceFile {
        &self.files[f.0 as usize]
    }

    #[inline]
    /// Whether any file declares a `const` type parameter (only then do literals look for
    /// one in their contextual types).
    pub fn has_const_type_params(&self) -> bool {
        *self.cached.has_const_type_params.get_or_init(|| {
            self.files.iter().any(|f| {
                let ast = &f.ast;
                (1..=ast.node_count() as u32).any(|i| {
                    ts_r_ast::NodeId::from_raw(i).is_some_and(|n| {
                        matches!(*ast.node(n), ts_r_ast::Node::TypeParam { modifiers, .. }
                            if modifiers.contains(ts_r_ast::Modifiers::CONST))
                    })
                })
            })
        })
    }

    pub fn ast(&self, f: FileId) -> &'a Ast {
        &self.files[f.0 as usize].ast
    }

    #[inline]
    pub fn bound(&self, f: FileId) -> &'a BoundFile {
        &self.files[f.0 as usize].bound
    }

    #[inline]
    pub fn node(&self, l: Loc) -> &'a Node {
        self.files[l.file.0 as usize].ast.node(l.node)
    }

    #[inline]
    pub fn span(&self, l: Loc) -> Span {
        self.files[l.file.0 as usize].ast.span(l.node)
    }

    pub fn parent(&self, l: Loc) -> Option<Loc> {
        self.file(l.file)
            .parents()
            .parent(l.node)
            .map(|n| Loc::new(l.file, n))
    }

    /// The resolution of request `req` (specifier `spec`) of `file`, in the request's own
    /// mode (see [`crate::program::ProgramFiles::mode_resolutions`]).
    pub fn resolution(
        &self,
        file: FileId,
        spec: &str,
        req: &ts_r_binder::ModuleRequest,
    ) -> Option<&'a Option<Resolution>> {
        let f = self.file(file);
        let path = f.path.as_str();
        let found = if self.mode_resolutions.is_empty() {
            None
        } else {
            let m = crate::program::request_mode(self.opts.module, path, f.package_esm, req);
            self.mode_resolutions
                .get(&(path.to_owned(), spec.to_owned(), m))
        };
        found.or_else(|| self.resolutions.get(&(path.to_owned(), spec.to_owned())))
    }

    pub fn text(&self, l: Loc) -> &'a str {
        let f = self.file(l.file);
        f.ast.span(l.node).text(&f.text)
    }

    pub(crate) fn global_slot(&self, g: GlobalType) -> &OnceLock<Option<TypeId>> {
        &self.globals[g as usize]
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// What a resolution-stack entry is computing (for circularity detection).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Resolving {
    Type(SymId),
    DeclaredType(SymId),
    EnumMembers(SymId),
    BaseTypes(SymId),
    Return(SigId),
    Constraint(TypeId),
    /// The base constraint of a type parameter (`T extends T | string` is circular).
    BaseConstraint(TypeId),
    Members(TypeId),
    Node(Loc),
    WriteType(SymId),
    ApparentMembers(TypeId),
    ConditionalConstraint(TypeId),
}

/// Expression checks one outermost speculative check may make (see
/// [`Ck::enter_speculation`]).
pub(crate) const MAX_SPECULATIVE_WORK: u32 = 200_000;

/// Speculative expression checks one file may make in total: each outermost speculative
/// check is bounded by [`MAX_SPECULATIVE_WORK`], but nested generic calls start a new one
/// at every level (and each keeps the types it made). The largest file of the bench repos
/// makes about 21k.
pub(crate) const MAX_FILE_SPECULATIVE_WORK: u32 = 500_000;

/// Caches of [`Ck::discriminate_union_by`] (`func.rs`).
#[derive(Default)]
pub(crate) struct Discriminants {
    /// Optional property names of a union's members, sorted by text.
    pub(crate) optional: FxHashMap<TypeId, std::rc::Rc<[ts_r_types::StrId]>>,
    /// Property types per member of a union, for names that can discriminate it.
    pub(crate) columns: FxHashMap<(TypeId, ts_r_types::StrId), Option<DiscriminantColumn>>,
}

/// A property's type in each member of a union (`None` where a member lacks it).
pub(crate) type DiscriminantColumn = std::rc::Rc<[Option<TypeId>]>;

/// Per-thread checking state.
pub struct Ck<'c, 'a> {
    pub c: &'c Checker<'a>,
    /// The file whose diagnostics are being collected.
    pub file: FileId,
    pub diags: Vec<Diagnostic>,
    /// > 0 while checking speculatively (overload candidates, inference): no diagnostics.
    pub no_report: u32,
    pub(crate) node_types: FxHashMap<Loc, TypeId>,
    pub(crate) type_nodes: FxHashMap<Loc, TypeId>,
    pub(crate) resolving: Vec<(Resolving, bool)>,
    /// Conditional constraints being computed (nested; bounded, see `conditional_constraint`).
    pub(crate) cond_constraint_depth: u32,
    /// Type parameters of the calls whose type arguments are being inferred (outermost
    /// first): an inner call's contextual type mentions them before they are known.
    pub(crate) inferring_params: Vec<TypeId>,
    pub(crate) circular: FxHashSet<Resolving>,
    /// The first resolution cycle found through each symbol's type (the stack from its
    /// `Resolving::Type` entry up), for TS2502 (`circular_annotations`).
    pub(crate) type_cycles: FxHashMap<SymId, Vec<Resolving>>,
    /// The first cycle found through each inferred return type (as `type_cycles`), for
    /// every return type on a cycle that closes at a return type.
    pub(crate) return_cycles: FxHashMap<SigId, Vec<Resolving>>,
    /// Types whose base constraint is being computed (`Ck::base_constraint`).
    pub(crate) base_constraints: Vec<TypeId>,
    pub(crate) flow_cache: FxHashMap<crate::flow::FlowKey, TypeId>,
    /// Flow types computed while a loop or speculation is pending (not final).
    pub(crate) flow_temp: FxHashMap<crate::flow::FlowKey, (TypeId, u32)>,
    /// Loop labels being iterated, with their current (incomplete) types.
    pub(crate) flow_pending: Vec<(crate::flow::FlowKey, TypeId)>,
    pub(crate) flow_loop: u32,
    /// Expression types computed inside a pending flow loop. Like `flow_temp`, each entry
    /// records the loop nesting level (`flow_pending.len()`) it was computed at; a loop at
    /// level k forgets entries of level >= k whenever its assumption changes.
    pub(crate) loop_node_types: FxHashMap<Loc, (TypeId, u32)>,
    /// `finally` labels restricted while walking through a reduce label.
    pub(crate) flow_reduce: Vec<(ts_r_binder::FlowId, Vec<ts_r_binder::FlowId>)>,
    pub(crate) ref_keys: FxHashMap<Loc, Option<crate::flow::RefKey>>,
    /// What a const binding aliases (`const { kind } = v` is `v.kind`).
    pub(crate) binding_aliases: FxHashMap<SymId, Option<crate::flow::BindingAlias>>,
    /// The aliased reference's root name for identifiers in conditions.
    pub(crate) alias_roots: FxHashMap<Loc, Option<Box<str>>>,
    /// Aliased conditions being inlined while narrowing (tsc stops at five).
    pub(crate) cond_alias_depth: u32,
    /// Return-context mappers of the calls whose arguments are being inferred (innermost
    /// last): a contextual return type that is one of their type parameters reads as what
    /// the call's contextual type implies for it.
    pub(crate) ret_ctx_mappers: Vec<ts_r_types::MapperId>,
    /// Set while resolving a type node in a deferred position (an array or tuple element,
    /// a type argument of a class or interface): a reference there to a non-generic type
    /// alias that is being resolved reads as a forward reference to it.
    pub(crate) deferred_alias_ref: bool,
    /// Aliases handed out as forward references before their resolution started; resolved
    /// (and linked) once no type alias is being resolved.
    pub(crate) pending_forwards: Vec<SymId>,
    pub(crate) assigned_syms: FxHashSet<SymId>,
    /// The start of the last assignment (not declaration) to each assigned symbol.
    pub(crate) last_assignment: FxHashMap<SymId, u32>,
    pub(crate) assigned_syms_files: FxHashSet<FileId>,
    pub(crate) call_kinds: FxHashMap<Loc, crate::flow::CallKindCache>,
    /// Final reachability of statement flows (with exhaustive switches, within a function).
    pub(crate) reachable_flows: FxHashMap<(FileId, ts_r_binder::FlowId), bool>,
    /// TS7027 runs the checker reported (`check_unreachable_stmts`): nested lists inside
    /// one are not reported again.
    pub(crate) unreachable_runs: Vec<(FileId, ts_r_ast::Span)>,
    /// `{ name: unknown }` objects that `"name" in x` intersects into `x`, per name.
    pub(crate) in_records: FxHashMap<String, TypeId>,
    /// `typeof globalThis`, built on first use.
    pub(crate) global_this: Option<TypeId>,
    /// Whether a `switch` (without `default`) covers every value of its discriminant.
    pub(crate) exhaustive_switches: FxHashMap<Loc, bool>,
    /// Expressions already checked with reporting on.
    pub(crate) checked: FxHashSet<Loc>,
    /// Function and class bodies to check after the current statement walk.
    pub(crate) deferred: Vec<Loc>,
    pub(crate) deferred_set: FxHashSet<Loc>,
    /// > 0 while checking speculatively (results are not cached).
    pub(crate) speculative: u32,
    /// Function expressions being checked speculatively (their contextual parameter types
    /// installed), innermost last, with the types of their locals that depend on those
    /// parameters: valid while the frame lasts, never published.
    pub(crate) spec_frames: Vec<(Loc, FxHashMap<SymId, TypeId>)>,
    /// Types of function expression arguments checked speculatively, by argument,
    /// contextual type and the parameter types around it (see `speculative_fn_key`).
    pub(crate) spec_fn_cache: FxHashMap<(Loc, TypeId, u64), TypeId>,
    /// Expressions checked in the current outermost speculative check (see
    /// [`Ck::enter_speculation`]).
    pub(crate) spec_work: u32,
    /// Speculative expression checks in the current file (bounded by
    /// [`MAX_FILE_SPECULATIVE_WORK`]).
    pub(crate) file_spec_work: u32,
    /// Whether the current file already reported running out of speculative work.
    pub(crate) spec_reported: bool,
    /// Set when a speculative check ran out of work (reported as TS2589 at the next
    /// expression checked outside speculation).
    pub(crate) spec_overflow: bool,
    /// Identifies the current speculative assumptions (which speculation, contextual type
    /// overrides and speculative parameter types are in force): a fresh id on every
    /// change ([`Ck::push_assumptions`]) and the previous one back when the change is
    /// undone ([`Ck::pop_assumptions`]). Speculative results are reused only under the
    /// same id (`spec_node_types`, `flow_temp`).
    pub(crate) spec_epoch: u64,
    pub(crate) epoch_counter: u64,
    pub(crate) epoch_stack: Vec<u64>,
    /// The `spec_epoch` the speculative entries of `flow_temp` were computed under.
    pub(crate) flow_temp_epoch: u64,
    /// Expression types computed while speculating, with the assumptions they hold under;
    /// cleared when the outermost speculation ends.
    pub(crate) spec_node_types: FxHashMap<Loc, (u64, TypeId)>,
    /// Types of function expressions.
    pub(crate) fn_types: FxHashMap<Loc, TypeId>,
    /// Widened object literal types.
    pub(crate) widened: FxHashMap<TypeId, TypeId>,
    /// [`Ck::reduced_type`] results.
    pub(crate) reduced: FxHashMap<TypeId, TypeId>,
    pub(crate) flow_depth: u32,
    pub(crate) depth: u32,
    /// Stack of contextual types for expressions being checked: `(expression, type)`.
    pub(crate) context_overrides: Vec<(Loc, TypeId)>,
    /// Contextual types of function arguments of overloaded calls, fixed by the first
    /// candidate that checked them (`None`: no context after every overload failed). They
    /// win over `context_overrides`: later candidates see the function as first typed.
    pub(crate) fixed_arg_contexts: FxHashMap<Loc, Option<TypeId>>,
    /// Context-sensitive functions nested in an overloaded call's argument while a
    /// candidate first checks the arguments without them: each reads as `any`.
    pub(crate) placeholder_fns: FxHashSet<Loc>,
    /// Set while instantiating the result of a failed overloaded call: inference skips
    /// context-sensitive arguments.
    pub(crate) skip_sensitive_inference: bool,
    /// Inferred return types of function-likes under a contextual signature (not shared).
    pub(crate) local_returns: FxHashMap<Loc, TypeId>,
    /// Node-level facts reported once (e.g. "declared but never read").
    pub(crate) reported: FxHashSet<(Loc, u32)>,
    /// Relation recursion state.
    pub(crate) rel: crate::relate::RelState,
    /// Assumed types of parameters while checking a function expression under a contextual
    /// signature.
    pub(crate) param_types: FxHashMap<Loc, TypeId>,
    /// Arguments of immediately invoked functions whose contextual type was needed while
    /// the call was being resolved (`(f => f(1))(i => i)`): they have no contextual type.
    pub(crate) circular_iife_args: FxHashSet<Loc>,
    /// Arguments being checked to type an immediately invoked function's parameters.
    pub(crate) iife_args: Vec<Loc>,
    /// Whether each function's parameter list contains syntax the target lowers.
    pub(crate) lowered_params: FxHashMap<Loc, bool>,
    /// Signatures resolved at call sites.
    pub(crate) resolved_calls: FxHashMap<Loc, Option<SigId>>,
    /// Set by overload resolution when no signature fit the call.
    pub(crate) overload_failed: bool,
    pub(crate) outer_cache: FxHashMap<Loc, Vec<TypeId>>,
    /// Tuple types of array literals in const contexts (they combine as inference
    /// candidates like object literals).
    pub(crate) const_array_literals: rustc_hash::FxHashSet<TypeId>,
    /// The contextual type a function argument that is not context sensitive first had
    /// while its call's type arguments were inferred, by argument and generic signature.
    pub(crate) first_arg_context: FxHashMap<(Loc, SigId), TypeId>,
    /// Outer type parameters a type literal or function type may name.
    pub(crate) outer_ref_cache: FxHashMap<Loc, Vec<TypeId>>,
    pub(crate) inst_syms: FxHashMap<(SymId, ts_r_types::MapperId), SymId>,
    pub(crate) inst_cache: FxHashMap<(TypeId, ts_r_types::MapperId), TypeId>,
    pub(crate) inst_depth: u32,
    /// Set when an instantiation hit the depth or count limit (reported as TS2589 at the
    /// next type reference or expression being checked).
    pub(crate) inst_overflow: bool,
    pub(crate) inst_count: u32,
    /// Conditional roots being resolved with the plain error type inferred into their
    /// `infer` parameters; re-entering one (`Awaited<error>`) resolves to the error type.
    pub(crate) error_infer_roots: Vec<u32>,
    /// Set when a tuple type would exceed 10,000 elements (TS2799 / TS2800).
    pub(crate) tuple_overflow: bool,
    /// `infer` type parameters printed as `infer X` (while printing an extends clause).
    pub(crate) print_infer: Vec<TypeId>,
    pub(crate) sig_inst_cache: FxHashMap<(SigId, ts_r_types::MapperId), SigId>,
    pub(crate) decl_sigs: FxHashMap<Loc, SigId>,
    pub(crate) union_props: FxHashMap<(TypeId, ts_r_types::StrId), Option<SymId>>,
    /// Unions built without subtype reduction, by their written members (`unions.rs`):
    /// sorting big member lists is the expensive part, and the same lists come back
    /// (constraints of indexed accesses). Pure in its key, so kept across files.
    pub(crate) union_cache: crate::unions::UnionCache,
    /// Per union contextual type: what discriminating object literals against it needs
    /// (`func.rs`). Pure in the type, so kept across files.
    pub(crate) discriminants: Discriminants,
    /// When set, printed type names drop the namespaces enclosing this node (tsc prints
    /// names relative to the error location). Only declaration checks set it.
    pub(crate) print_scope: Option<Loc>,
    /// When set, printed type names carry their enclosing namespaces (`N.I`). tsc 7 prints
    /// bare names except to tell apart two types that would print alike, and for an enum
    /// type widened from one of its members.
    pub(crate) print_qualified: bool,
    /// Types printed so far by the outermost `print_type` call (a budget against nested
    /// generic instantiations that double at each level).
    pub(crate) print_nodes: u32,
    /// Per file, the variables assigned compound-like (`x = x + 1`, `x += 1`), whose
    /// literal initializer types widen (`symtype.rs`).
    pub(crate) compound_assigned: FxHashMap<FileId, std::rc::Rc<FxHashSet<SymId>>>,
    /// The sources and mapped types being reverse-mapped (`mapped.rs`): a recursive
    /// mapped type meeting the same source again reverse-maps it to `any`.
    pub(crate) reverse_mapping: Vec<(TypeId, Loc)>,
    /// Fresh object literal types with spreads, by their literal (`expr.rs`): only the
    /// properties written in that literal are checked for excess (`relate.rs`).
    pub(crate) spread_literals: FxHashMap<TypeId, Loc>,
    /// JSX namespaces, runtimes and resolved elements (`jsx_check.rs`).
    pub(crate) jsx: crate::jsx_check::JsxState,
}

impl std::fmt::Debug for Ck<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ck")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    pub fn new(c: &'c Checker<'a>, file: FileId) -> Ck<'c, 'a> {
        Ck {
            c,
            file,
            diags: Vec::new(),
            no_report: 0,
            node_types: FxHashMap::default(),
            type_nodes: FxHashMap::default(),
            resolving: Vec::new(),
            cond_constraint_depth: 0,
            inferring_params: Vec::new(),
            circular: FxHashSet::default(),
            type_cycles: FxHashMap::default(),
            return_cycles: FxHashMap::default(),
            base_constraints: Vec::new(),
            flow_cache: FxHashMap::default(),
            flow_temp: FxHashMap::default(),
            flow_pending: Vec::new(),
            flow_loop: 0,
            loop_node_types: FxHashMap::default(),
            flow_reduce: Vec::new(),
            ref_keys: FxHashMap::default(),
            binding_aliases: FxHashMap::default(),
            alias_roots: FxHashMap::default(),
            cond_alias_depth: 0,
            ret_ctx_mappers: Vec::new(),
            deferred_alias_ref: false,
            pending_forwards: Vec::new(),
            assigned_syms: FxHashSet::default(),
            last_assignment: FxHashMap::default(),
            assigned_syms_files: FxHashSet::default(),
            call_kinds: FxHashMap::default(),
            reachable_flows: FxHashMap::default(),
            unreachable_runs: Vec::new(),
            in_records: FxHashMap::default(),
            global_this: None,
            exhaustive_switches: FxHashMap::default(),
            checked: FxHashSet::default(),
            deferred: Vec::new(),
            deferred_set: FxHashSet::default(),
            speculative: 0,
            fn_types: FxHashMap::default(),
            widened: FxHashMap::default(),
            reduced: FxHashMap::default(),
            flow_depth: 0,
            depth: 0,
            context_overrides: Vec::new(),
            fixed_arg_contexts: FxHashMap::default(),
            placeholder_fns: FxHashSet::default(),
            skip_sensitive_inference: false,
            local_returns: FxHashMap::default(),
            reported: FxHashSet::default(),
            rel: crate::relate::RelState::default(),
            param_types: FxHashMap::default(),
            circular_iife_args: FxHashSet::default(),
            iife_args: Vec::new(),
            lowered_params: FxHashMap::default(),
            resolved_calls: FxHashMap::default(),
            overload_failed: false,
            outer_cache: FxHashMap::default(),
            const_array_literals: Default::default(),
            first_arg_context: FxHashMap::default(),
            outer_ref_cache: FxHashMap::default(),
            inst_syms: FxHashMap::default(),
            inst_cache: FxHashMap::default(),
            inst_depth: 0,
            inst_overflow: false,
            inst_count: 0,
            spec_frames: Vec::new(),
            spec_fn_cache: FxHashMap::default(),
            spec_work: 0,
            file_spec_work: 0,
            spec_reported: false,
            spec_overflow: false,
            spec_epoch: 0,
            epoch_counter: 0,
            epoch_stack: Vec::new(),
            flow_temp_epoch: 0,
            spec_node_types: FxHashMap::default(),
            error_infer_roots: Vec::new(),
            tuple_overflow: false,
            print_infer: Vec::new(),
            sig_inst_cache: FxHashMap::default(),
            decl_sigs: FxHashMap::default(),
            union_props: FxHashMap::default(),
            print_scope: None,
            print_qualified: false,
            print_nodes: 0,
            compound_assigned: FxHashMap::default(),
            reverse_mapping: Vec::new(),
            spread_literals: FxHashMap::default(),
            union_cache: Default::default(),
            discriminants: Default::default(),
            jsx: Default::default(),
        }
    }

    /// Starts checking another file (keeps caches that are not file-specific).
    pub fn reset_for(&mut self, file: FileId) {
        self.file = file;
        self.diags.clear();
        self.node_types.clear();
        self.flow_cache.clear();
        self.flow_temp.clear();
        self.loop_node_types.clear();
        self.ref_keys.clear();
        self.binding_aliases.clear();
        self.alias_roots.clear();
        self.call_kinds.clear();
        self.reachable_flows.clear();
        self.unreachable_runs.clear();
        self.exhaustive_switches.clear();
        self.checked.clear();
        self.deferred.clear();
        self.deferred_set.clear();
        self.fn_types.clear();
        self.local_returns.clear();
        self.reported.clear();
        self.param_types.clear();
        self.lowered_params.clear();
        self.resolved_calls.clear();
        self.fixed_arg_contexts.clear();
        self.first_arg_context.clear();
        self.file_spec_work = 0;
        self.spec_reported = false;
        self.spec_overflow = false;
    }

    #[inline]
    pub fn store(&self) -> &'c TypeStore {
        &self.c.store
    }

    #[inline]
    pub fn ast(&self, f: FileId) -> &'a Ast {
        self.c.ast(f)
    }

    #[inline]
    pub fn node(&self, l: Loc) -> &'a Node {
        self.c.node(l)
    }

    /// Whether diagnostics at `l` are kept now.
    #[inline]
    pub fn reporting(&self, file: FileId) -> bool {
        self.no_report == 0 && file == self.file
    }

    /// Reports `info(args)` at `span` of `file`.
    pub fn error_span(
        &mut self,
        file: FileId,
        span: Span,
        info: dg::DiagnosticInfo,
        args: &[&str],
    ) {
        if !self.reporting(file) {
            return;
        }
        self.diags
            .push(Diagnostic::new(msg(info), span, args).with_file(file));
    }

    /// Reports at the span of node `l`.
    pub fn error(&mut self, l: Loc, info: dg::DiagnosticInfo, args: &[&str]) {
        if !self.reporting(l.file) {
            return;
        }
        let span = self.error_span_of(l);
        self.diags
            .push(Diagnostic::new(msg(info), span, args).with_file(l.file));
    }

    /// Reports a message chain at node `l`.
    pub fn error_chain(&mut self, l: Loc, chain: &Chain) {
        if !self.reporting(l.file) || chain.is_empty() {
            return;
        }
        let span = self.error_span_of(l);
        self.push_chain(l.file, span, chain);
    }

    /// Attaches related information at `at` (its error span: a declaration's name) to the
    /// diagnostic reported last, when one was reported since `mark` (`self.diags.len()`
    /// before reporting).
    pub fn relate_last(&mut self, mark: usize, at: Loc, info: dg::DiagnosticInfo, args: &[&str]) {
        let span = self.related_span_of(at);
        self.relate_last_span(mark, at.file, span, info, args);
    }

    /// The span related information points at for node `l`: [`Ck::error_span_of`], except
    /// that signature members (methods, call, construct and index signatures) are
    /// whole (observed: "The last overload is declared here." under a method signature).
    pub fn related_span_of(&self, l: Loc) -> Span {
        let ast = self.ast(l.file);
        match ast.node(l.node) {
            Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::IndexSignature { .. } => ast.span(l.node),
            _ => self.error_span_of(l),
        }
    }

    /// [`Ck::relate_last`] at an explicit span.
    pub fn relate_last_span(
        &mut self,
        mark: usize,
        file: FileId,
        span: Span,
        info: dg::DiagnosticInfo,
        args: &[&str],
    ) {
        if self.diags.len() <= mark {
            return;
        }
        let m = msg(info);
        if let Some(d) = self.diags.last_mut() {
            d.related.push(ts_r_diagnostics::RelatedInfo {
                code: m.code,
                category: m.category,
                message: m.format(args),
                file,
                span,
            });
        }
    }

    pub fn push_chain(&mut self, file: FileId, span: Span, chain: &Chain) {
        if !self.reporting(file) || chain.is_empty() {
            return;
        }
        let code = chain.code();
        let info = crate::diagnostics_generated::lookup(code);
        let category = info.map_or(ts_r_diagnostics::Category::Error, |i| msg(*i).category);
        self.diags.push(Diagnostic {
            code,
            category,
            message: chain.text(),
            file,
            span,
            related: Vec::new(),
        });
    }

    /// The span tsc reports for a node: declarations report at their name, and leading
    /// trivia is never included (spans are already token-tight).
    pub fn error_span_of(&self, l: Loc) -> Span {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::VarDeclarator { name, .. } => ast.span(name),
            Node::FunctionDecl { name: Some(n), .. } | Node::ClassDecl { name: Some(n), .. } => {
                ast.span(n)
            }
            Node::Interface { name, .. }
            | Node::TypeAlias { name, .. }
            | Node::Enum { name, .. } => ast.span(name),
            Node::PropertyDef { key, .. }
            | Node::PropertySignature { key, .. }
            | Node::MethodSignature { key, .. } => ast.span(key),
            Node::MethodDef { key, .. } => ast.span(key),
            // Return statements report at the `return` keyword.
            Node::Return { .. } => {
                let s = ast.span(l.node);
                Span::new(s.start, (s.start + 6).min(s.end))
            }
            _ => ast.span(l.node),
        }
    }

    /// Starts a speculative check (nothing is cached or reported until the matching
    /// [`Ck::leave_speculation`]). The outermost one starts a budget of
    /// [`MAX_SPECULATIVE_WORK`] expression checks: speculation re-checks what it does not
    /// cache, so nested speculation can grow exponentially, and past the budget every
    /// expression is unresolved and the overflow is reported as TS2589.
    pub(crate) fn enter_speculation(&mut self) {
        if self.speculative == 0 {
            self.spec_work = 0;
        }
        self.speculative += 1;
        self.push_assumptions();
    }

    pub(crate) fn leave_speculation(&mut self) {
        self.speculative -= 1;
        self.pop_assumptions();
        if self.speculative == 0 {
            self.spec_node_types.clear();
        }
    }

    /// Starts a new set of speculative assumptions (see `spec_epoch`).
    pub(crate) fn push_assumptions(&mut self) {
        self.epoch_stack.push(self.spec_epoch);
        self.epoch_counter += 1;
        self.spec_epoch = self.epoch_counter;
    }

    /// Returns to the assumptions before the matching [`Ck::push_assumptions`].
    pub(crate) fn pop_assumptions(&mut self) {
        if let Some(e) = self.epoch_stack.pop() {
            self.spec_epoch = e;
        }
    }

    /// Runs `f` without reporting diagnostics.
    pub fn quietly<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.no_report += 1;
        let r = f(self);
        self.no_report -= 1;
        r
    }

    /// Pushes a resolution target; `false` when it is already being computed (a cycle).
    pub fn push_resolution(&mut self, r: Resolving) -> bool {
        if let Some(i) = self.resolving.iter().position(|(x, _)| *x == r) {
            for e in &mut self.resolving[i..] {
                e.1 = true;
            }
            if let Resolving::Type(s) = r
                && !self.type_cycles.contains_key(&s)
            {
                let path = self.resolving[i..].iter().take(256).map(|e| e.0).collect();
                self.type_cycles.insert(s, path);
            }
            if let Resolving::Return(_) = r {
                let path: Vec<Resolving> =
                    self.resolving[i..].iter().take(256).map(|e| e.0).collect();
                for e in &path {
                    if let Resolving::Return(s) = *e {
                        self.return_cycles.entry(s).or_insert_with(|| path.clone());
                    }
                }
            }
            return false;
        }
        self.resolving.push((r, false));
        true
    }

    /// Pops the top resolution; returns `false` if a cycle went through it.
    pub fn pop_resolution(&mut self) -> bool {
        match self.resolving.pop() {
            Some((r, circ)) => {
                if circ {
                    self.circular.insert(r);
                }
                !circ
            }
            None => true,
        }
    }

    pub fn is_resolving(&self, r: Resolving) -> bool {
        self.resolving.iter().any(|(x, _)| *x == r)
    }
}
