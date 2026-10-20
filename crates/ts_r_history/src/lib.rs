//! Incremental, history-based re-checking (design record `docs/design/0005-history.md`).
//!
//! * [`levels`]: which files can be checked once and shared (lib, dependencies, stable
//!   project files) and which are hot.
//! * [`layout`]: ordering a program by level and sharing the lower levels process-wide.

pub mod codec;
pub mod incremental;
pub mod layout;
mod lazy;
pub mod levels;
mod libinfo;
pub mod persist;
pub mod shape;
pub mod stat;

pub use layout::{Arranged, arrange, live_layers};

use ts_r_checker::{CheckResult, CheckRun, Checker, ProgramFiles, check_program_with};

/// The implicit `tslib` import, explicit module requests, then the implicit JSX runtime
/// import, as loaded.
pub(crate) fn module_requests<'a>(
    f: &'a ts_r_checker::SourceFile,
    options: &'a ts_r_config::CompilerOptions,
) -> impl Iterator<Item = (std::borrow::Cow<'a, str>, ts_r_binder::RequestKind)> {
    ts_r_checker::emit_helpers::helpers_specifier(f, options)
        .map(|s| {
            (
                std::borrow::Cow::Borrowed(s),
                ts_r_binder::RequestKind::Import,
            )
        })
        .into_iter()
        .chain(
            f.bound
                .module_requests
                .iter()
                .map(|r| (std::borrow::Cow::Borrowed(f.ast.atom(r.specifier)), r.kind)),
        )
        .chain(
            ts_r_checker::jsx_runtime::runtime_specifier(f, options)
                .map(|s| (std::borrow::Cow::Owned(s), ts_r_binder::RequestKind::Import)),
        )
}

/// Requests with the mode of their separate resolution, or `None` for the specifier's
/// first (unattributed) mode, as in `load_program`.
pub(crate) fn module_requests_with_modes<'a>(
    f: &'a ts_r_checker::SourceFile,
    options: &'a ts_r_config::CompilerOptions,
) -> impl Iterator<
    Item = (
        std::borrow::Cow<'a, str>,
        ts_r_binder::RequestKind,
        Option<ts_r_resolver::ResolutionMode>,
    ),
> {
    let mut first = rustc_hash::FxHashMap::default();
    let module = options.effective().module();
    let helpers = ts_r_checker::emit_helpers::helpers_specifier(f, options).map(|s| {
        (
            std::borrow::Cow::Borrowed(s),
            ts_r_binder::RequestKind::Import,
            None,
        )
    });
    helpers
        .into_iter()
        .chain(f.bound.module_requests.iter().map(move |r| {
            let spec = f.ast.atom(r.specifier);
            let base = ts_r_checker::program::request_mode(
                module,
                &f.path,
                f.package_esm,
                &ts_r_binder::ModuleRequest {
                    resolution_mode: None,
                    ..*r
                },
            );
            let first = *first.entry(spec).or_insert(base);
            let mode = r.resolution_mode.unwrap_or(base);
            (
                std::borrow::Cow::Borrowed(spec),
                r.kind,
                (mode != first).then_some(mode),
            )
        }))
        .chain(
            ts_r_checker::jsx_runtime::runtime_specifier(f, options).map(|s| {
                (
                    std::borrow::Cow::Owned(s),
                    ts_r_binder::RequestKind::Import,
                    None,
                )
            }),
        )
}

pub(crate) fn request_resolution<'a>(
    program: &'a ProgramFiles,
    f: &ts_r_checker::SourceFile,
    spec: &str,
    mode: Option<ts_r_resolver::ResolutionMode>,
) -> Option<&'a Option<ts_r_checker::program::Resolution>> {
    mode.and_then(|mode| {
        program
            .mode_resolutions
            .get(&(f.path.clone(), spec.to_owned(), mode))
    })
    .or_else(|| program.resolutions.get(&(f.path.clone(), spec.to_owned())))
}

/// Checks `program` on shared lower levels with `hot` (file indices) as the edited files.
/// Returns the arranged program (the diagnostics' file ids index it) and the result, which
/// equals `check_program`'s up to file ids.
pub fn check_layered(
    program: &ProgramFiles,
    hot: &[usize],
    run: CheckRun,
) -> (ProgramFiles, CheckResult) {
    let mut p = levels::with_files(program, program.files.clone());
    let arranged = arrange(&mut p, hot);
    let result = check_program_with(&p, run, |p| {
        Checker::layered(p, arranged.parent.as_ref()).unwrap_or_else(|_| Checker::new(p))
    });
    (p, result)
}
