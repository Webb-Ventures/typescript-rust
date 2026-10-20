//! Up-to-date statuses of `--build` (design record 0012, E3) and their messages.
//!
//! The order of the checks, observed in tsc 7.0.2's `tsbuild` baselines: dependency
//! failures (`--stopBuildOnErrors`), `--force`, the buildinfo (missing, other version,
//! errors to report, emit pending, options changed, a root file dropped), then inputs
//! against outputs, the config files, and finally the referenced projects' outputs.
//! Incremental projects compare against their buildinfo alone; others also need every
//! output to exist. An input that is newer than the buildinfo but has the stored content
//! only needs its outputs' timestamps updated; so does a project whose references rebuilt
//! without changing a declaration file.

use std::time::SystemTime;

use ts_r_config::ParsedConfig;
use ts_r_resolver::ProjectReferences;

use crate::buildinfo::{self, BuildInfo, Read};
use crate::incremental::{emit_kinds, option_hashes};

/// Why a project is (not) built. Paths are absolute; [`Status::message`] shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    ConfigMissing,
    UpstreamErrors {
        dep: String,
    },
    UpstreamNotBuilt {
        dep: String,
    },
    Forced,
    OutputMissing {
        output: String,
    },
    OtherVersion {
        buildinfo: String,
        version: String,
    },
    ReportErrors {
        buildinfo: String,
    },
    PendingEmit {
        buildinfo: String,
    },
    OptionsChanged {
        buildinfo: String,
    },
    RootRemoved {
        buildinfo: String,
        file: String,
    },
    InputMissing {
        input: String,
    },
    OlderThanInput {
        output: String,
        input: String,
    },
    /// Up to date, but some outputs need newer timestamps (inputs touched, not changed).
    TouchForInputs,
    /// Up to date, references rebuilt without declaration changes: update timestamps.
    TouchForUpstream,
    UpToDate {
        newest_input: String,
        output: String,
    },
    /// No inputs (a solution of references, or an explicitly empty `files`): nothing is
    /// built; only the config's own errors are reported ([`is_container`]).
    Container,
    /// `--watch`: the last build of this session reported errors and a reference changed
    /// (never computed by [`check`]; the watch cycle decides it).
    HasErrors,
}

impl Status {
    /// A build is needed.
    pub fn needs_build(&self) -> bool {
        !matches!(
            self,
            Status::TouchForInputs
                | Status::TouchForUpstream
                | Status::UpToDate { .. }
                | Status::Container
                | Status::UpstreamErrors { .. }
                | Status::UpstreamNotBuilt { .. }
        )
    }

    pub fn needs_touch(&self) -> bool {
        matches!(self, Status::TouchForInputs | Status::TouchForUpstream)
    }

    /// The verbose message, paths shown by `show`.
    pub fn message(&self, project: &str, show: &dyn Fn(&str) -> String) -> Option<String> {
        let p = show(project);
        Some(match self {
            Status::ConfigMissing => {
                format!("Project '{p}' is out of date because config file does not exist.")
            }
            Status::UpstreamErrors { dep } => format!(
                "Project '{p}' can't be built because its dependency '{}' has errors",
                show(dep)
            ),
            Status::UpstreamNotBuilt { dep } => format!(
                "Project '{p}' can't be built because its dependency '{}' was not built",
                show(dep)
            ),
            Status::Forced => format!("Project '{p}' is being forcibly rebuilt"),
            Status::OutputMissing { output } => format!(
                "Project '{p}' is out of date because output file '{}' does not exist",
                show(output)
            ),
            Status::OtherVersion { version, .. } => format!(
                "Project '{p}' is out of date because output for it was generated with version '{version}' that differs with current version '{}'",
                buildinfo::current_version()
            ),
            Status::ReportErrors { buildinfo } => format!(
                "Project '{p}' is out of date because buildinfo file '{}' indicates that program needs to report errors.",
                show(buildinfo)
            ),
            Status::PendingEmit { buildinfo } => format!(
                "Project '{p}' is out of date because buildinfo file '{}' indicates that some of the changes were not emitted",
                show(buildinfo)
            ),
            Status::OptionsChanged { buildinfo } => format!(
                "Project '{p}' is out of date because buildinfo file '{}' indicates there is change in compilerOptions",
                show(buildinfo)
            ),
            Status::RootRemoved { buildinfo, file } => format!(
                "Project '{p}' is out of date because buildinfo file '{}' indicates that file '{}' was root file of compilation but not any more.",
                show(buildinfo),
                show(file)
            ),
            Status::InputMissing { input } => format!(
                "Project '{p}' is out of date because input '{}' does not exist.",
                show(input)
            ),
            Status::OlderThanInput { output, input } => format!(
                "Project '{p}' is out of date because output '{}' is older than input '{}'",
                show(output),
                show(input)
            ),
            Status::TouchForInputs => format!(
                "Project '{p}' is up to date but needs to update timestamps of output files that are older than input files"
            ),
            Status::TouchForUpstream => {
                format!("Project '{p}' is up to date with .d.ts files from its dependencies")
            }
            Status::UpToDate {
                newest_input,
                output,
            } => format!(
                "Project '{p}' is up to date because newest input '{}' is older than output '{}'",
                show(newest_input),
                show(output)
            ),
            Status::HasErrors => format!("Project '{p}' is out of date because it has errors."),
            Status::Container => return None,
        })
    }
}

/// A referenced project, as the status check sees it.
#[derive(Debug, Clone)]
pub struct Upstream {
    /// The reference's path, resolved (tsc names the dependency by it).
    pub shown: String,
    /// Its config file (a `--watch` build names the dependency by it).
    pub config: String,
    pub buildinfo: String,
    /// This build: the project had errors / was not built (`None`: not built in this run).
    pub errors: bool,
    pub skipped: bool,
    /// Compiled in this run (whether or not it wrote anything).
    pub built: bool,
}

/// The referenced project `u` was built after `bi_time` and its latest changed declaration
/// output (or, unknown, any) is newer than `bi_time`.
fn upstream_dts_changed(
    u: &Upstream,
    bi_time: SystemTime,
    stored: &dyn Fn(&str) -> Option<String>,
) -> bool {
    if !u.built && mtime(&u.buildinfo).is_none_or(|t| t <= bi_time) {
        return false;
    }
    let latest = match buildinfo::read(&u.buildinfo) {
        Read::Ours(b) => b
            .program
            .and_then(|p| p.latest_changed_dts)
            .map(|d| buildinfo::absolute(&u.buildinfo, &d)),
        _ => None,
    };
    let Some(latest) = latest else {
        return true;
    };
    if mtime(&latest).is_none_or(|d| d <= bi_time) {
        return false;
    }
    // Rewritten since, but the text this project last read is the same.
    stored(&latest)
        .is_none_or(|h| read_text(&latest).map(|b| buildinfo::content_hash(&latest, &b)) != Some(h))
}

/// Everything [`check`] reads.
#[derive(Debug)]
pub struct Inputs<'a> {
    pub config: &'a ParsedConfig,
    pub upstream: &'a [Upstream],
    pub references: &'a ProjectReferences,
    pub force: bool,
    pub stop_on_errors: bool,
    /// A `--watch` build.
    pub watch: bool,
}

fn mtime(p: &str) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn read_text(p: &str) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

/// The status of one project.
pub fn check(i: &Inputs) -> Status {
    let c = i.config;
    for u in i.upstream {
        if i.stop_on_errors && u.errors {
            return Status::UpstreamErrors {
                dep: u.shown.clone(),
            };
        }
        if u.skipped {
            return Status::UpstreamNotBuilt {
                dep: u.shown.clone(),
            };
        }
    }
    if is_container(c) {
        return Status::Container;
    }
    if i.force {
        return Status::Forced;
    }
    let cs = c.case_sensitive();
    let bi_path = buildinfo::path_for(&c.options, &c.config_path);
    let incremental = buildinfo::is_incremental(&c.options);
    let info: BuildInfo = match buildinfo::read(&bi_path) {
        Read::Missing => return Status::OutputMissing { output: bi_path },
        Read::Foreign(v) => {
            return match v {
                Some(version) => Status::OtherVersion {
                    buildinfo: bi_path,
                    version,
                },
                None => Status::OutputMissing { output: bi_path },
            };
        }
        Read::Ours(b) => *b,
    };
    let Some(bi_time) = mtime(&bi_path) else {
        return Status::OutputMissing { output: bi_path };
    };
    // Semantic errors and files a `noCheck` build left unchecked matter only to a checking
    // build (observed).
    let no_check = c.options.no_check() == Some(true);
    if info.errors || (!no_check && (info.semantic_errors || info.check_pending)) {
        return Status::ReportErrors { buildinfo: bi_path };
    }
    // Output kinds asked for now that the last build did not produce (JavaScript after
    // `emitDeclarationOnly`, declarations turned on, source maps toggled): reported before
    // a newer config file (observed). Pending emit is reported below instead.
    if incremental
        && let Some(state) = info.program.as_ref()
        && state.pending.is_empty()
    {
        let kinds = emit_kinds(&c.options);
        let now = option_hashes(&c.options, &c.config_dir);
        let o = &state.options;
        let js =
            kinds & buildinfo::pending::JS != 0 && (o.js.is_none() || o.js_maps != now.js_maps);
        let dts =
            kinds & buildinfo::pending::DTS != 0 && (o.dts.is_none() || o.dts_maps != now.dts_maps);
        if js || dts {
            return Status::OptionsChanged { buildinfo: bi_path };
        }
    }
    // A root the buildinfo lists that is no longer one: before a missing output (observed:
    // a referenced project's buildinfo at the same path, TS6377).
    let roots: Vec<String> = c
        .file_names
        .iter()
        .map(|f| buildinfo::relative_to(&bi_path, f, cs))
        .collect();
    if let Some(gone) = info.root.iter().find(|r| !roots.contains(r)) {
        return Status::RootRemoved {
            file: buildinfo::absolute(&bi_path, gone),
            buildinfo: bi_path,
        };
    }
    // A missing output of a project without program state is reported before a newer
    // config file (observed: `tsbuildWatch/noEmit/semantic-errors`, turning `noEmit` off),
    // but after a newer root file (`tsbuild/noEmit/dts-errors-without-dts-enabled`).
    // So is an output older than a root file (`tsbuildWatch/noEmit/dts-errors-without-dts-
    // enabled`). With program state, so is pending emit (`semantic-errors-with-incremental`).
    if !incremental
        && !c
            .file_names
            .iter()
            .any(|f| mtime(f).is_none_or(|t| t > bi_time))
    {
        let mut oldest: Option<(String, SystemTime)> = None;
        for out in expected_outputs(c) {
            let Some(t) = mtime(&out) else {
                return Status::OutputMissing { output: out };
            };
            if oldest.as_ref().is_none_or(|(_, o)| t < *o) {
                oldest = Some((out, t));
            }
        }
        // Config files are inputs too when the buildinfo holds program state, written while
        // the project was incremental (observed: `tsbuildWatch/programUpdates/works-with-
        // extended-source-files`, a project that stopped being composite); otherwise a newer
        // config is reported against the buildinfo below (`tsbuildWatch/noEmit`).
        let configs: &[String] = if info.program.is_some() {
            std::slice::from_ref(&c.config_path)
        } else {
            &[]
        };
        let extended: &[String] = if info.program.is_some() {
            &c.extended_config_paths
        } else {
            &[]
        };
        let newest = c
            .file_names
            .iter()
            .chain(configs)
            .chain(extended)
            .filter_map(|f| Some((f, mtime(f)?)))
            .max_by_key(|(_, t)| *t);
        if let (Some((input, nt)), Some((output, ot))) = (newest, oldest)
            && nt > ot
        {
            return Status::OlderThanInput {
                output,
                input: input.clone(),
            };
        }
    }
    if incremental
        && let Some(state) = info.program.as_ref()
        && state
            .pending
            .iter()
            .any(|&(_, k)| k & emit_kinds(&c.options) != 0)
    {
        return Status::PendingEmit { buildinfo: bi_path };
    }
    // Config files.
    for cfg in std::iter::once(&c.config_path).chain(&c.extended_config_paths) {
        if mtime(cfg).is_some_and(|t| t > bi_time) {
            return Status::OlderThanInput {
                output: bi_path.clone(),
                input: cfg.clone(),
            };
        }
    }
    let state = info.program.as_ref().filter(|_| incremental);
    if incremental && state.is_none() {
        // A buildinfo without program state for an incremental project.
        return Status::OptionsChanged { buildinfo: bi_path };
    }
    if let Some(state) = state {
        let kinds = emit_kinds(&c.options);
        if state.pending.iter().any(|&(_, k)| k & kinds != 0) {
            return Status::PendingEmit { buildinfo: bi_path };
        }
        let now = option_hashes(&c.options, &c.config_dir);
        let js = kinds & buildinfo::pending::JS != 0
            && state.options.js.is_some()
            && state.options.js != now.js;
        let dts = kinds & buildinfo::pending::DTS != 0
            && state.options.dts.is_some()
            && state.options.dts != now.dts;
        if state.options.check != now.check || js || dts {
            return Status::OptionsChanged { buildinfo: bi_path };
        }
    }
    // A project without program state reports a dropped root before stored program state
    // (observed: a referenced project's buildinfo at the same path, TS6377).
    if !incremental && program_options_changed(&info) {
        return Status::OptionsChanged { buildinfo: bi_path };
    }

    // Inputs.
    let stored = |abs: &str| -> Option<&str> {
        let rel = buildinfo::relative_to(&bi_path, abs, cs);
        state?
            .files
            .iter()
            .find(|f| f.path == rel)
            .map(|f| f.hash.as_str())
    };
    let mut newest: Option<(String, SystemTime)> = None;
    let mut touch = false;
    for f in &c.file_names {
        let Some(t) = mtime(f) else {
            return Status::InputMissing { input: f.clone() };
        };
        if newest.as_ref().is_none_or(|(_, n)| t > *n) {
            newest = Some((f.clone(), t));
        }
    }
    if let Some((input, t)) = &newest
        && *t > bi_time
    {
        if incremental {
            // Unchanged text only needs timestamps.
            for f in &c.file_names {
                if mtime(f).is_some_and(|t| t > bi_time) {
                    let real = i.references.redirect(f).map_or(f.as_str(), |(dts, _)| dts);
                    let now = read_text(real).map(|b| buildinfo::content_hash(real, &b));
                    if now.as_deref() != stored(real) || now.is_none() {
                        return Status::OlderThanInput {
                            output: bi_path,
                            input: f.clone(),
                        };
                    }
                    touch = true;
                }
            }
        } else {
            return Status::OlderThanInput {
                output: bi_path,
                input: input.clone(),
            };
        }
    }
    let stored_owned = |abs: &str| stored(abs).map(str::to_owned);
    if let Some(state) = state {
        let root_set: Vec<&String> = roots.iter().collect();
        for f in &state.files {
            if root_set.contains(&&f.path) {
                continue;
            }
            let abs = buildinfo::absolute(&bi_path, &f.path);
            match mtime(&abs) {
                None => return Status::InputMissing { input: abs },
                Some(t) if t > bi_time => {
                    if read_text(&abs)
                        .map(|b| buildinfo::content_hash(&abs, &b))
                        .as_deref()
                        != Some(f.hash.as_str())
                    {
                        return Status::OlderThanInput {
                            output: bi_path,
                            input: abs,
                        };
                    }
                    touch = true;
                }
                Some(_) => {}
            }
        }
    }
    // `package.json` files resolution read or looked for (observed: tsc 7 records both).
    if let Some(state) = state {
        for p in &state.package_jsons {
            let abs = buildinfo::absolute(&bi_path, p);
            if mtime(&abs).is_none_or(|t| t > bi_time) {
                return Status::OlderThanInput {
                    output: bi_path,
                    input: abs,
                };
            }
        }
        for p in &state.missing_package_jsons {
            let abs = buildinfo::absolute(&bi_path, p);
            if mtime(&abs).is_some() {
                return Status::OlderThanInput {
                    output: bi_path,
                    input: abs,
                };
            }
        }
    }
    // Outputs of projects without program state must all exist.
    let mut oldest: Option<(String, SystemTime)> = None;
    if !incremental {
        for out in expected_outputs(c) {
            let Some(t) = mtime(&out) else {
                return Status::OutputMissing { output: out };
            };
            if oldest.as_ref().is_none_or(|(_, o)| t < *o) {
                oldest = Some((out, t));
            }
        }
        if let (Some((input, nt)), Some((output, ot))) = (&newest, &oldest)
            && nt > ot
        {
            return Status::OlderThanInput {
                output: output.clone(),
                input: input.clone(),
            };
        }
    }
    // Referenced projects' outputs.
    let mut upstream_touch = false;
    for u in i.upstream {
        if !u.built && mtime(&u.buildinfo).is_none_or(|t| t <= bi_time) {
            continue;
        }
        if upstream_dts_changed(u, bi_time, &stored_owned) {
            // A watch build names the project's oldest output and the reference's config
            // (observed: `tsbuildWatch/projectsBuilding`, `tsbuildWatch/reexport`).
            if i.watch {
                let mut oldest: Option<(String, SystemTime)> = None;
                for out in expected_outputs(c).into_iter().chain([bi_path.clone()]) {
                    if let Some(t) = mtime(&out)
                        && oldest.as_ref().is_none_or(|(_, o)| t < *o)
                    {
                        oldest = Some((out, t));
                    }
                }
                return Status::OlderThanInput {
                    output: oldest.map_or(bi_path, |(o, _)| o),
                    input: u.config.clone(),
                };
            }
            return Status::OlderThanInput {
                output: bi_path,
                input: u.shown.clone(),
            };
        }
        upstream_touch = true;
    }
    if upstream_touch {
        return Status::TouchForUpstream;
    }
    if touch {
        return Status::TouchForInputs;
    }
    let output = oldest.map_or(bi_path, |(o, _)| o);
    match newest {
        Some((newest_input, _)) => Status::UpToDate {
            newest_input,
            output,
        },
        None => Status::Container,
    }
}

/// A project that is never built: no inputs, and either references (a solution) or an
/// explicit `files` list (tsc's TS18002 project: its error is reported, nothing is written,
/// observed in `has-empty-files-diagnostic-when-files-is-empty-and-no-references-...`).
pub fn is_container(c: &ParsedConfig) -> bool {
    c.file_names.is_empty() && (c.declares_references || c.files.is_some())
}

/// A buildinfo without program state written for a project that now keeps one, or the
/// reverse, means the options changed.
fn program_options_changed(info: &BuildInfo) -> bool {
    info.program.is_some()
}

/// The outputs a project without program state writes, per root file in order (JavaScript,
/// its map, declaration, its map), for the up-to-date check and `--clean`.
pub fn expected_outputs(c: &ParsedConfig) -> Vec<String> {
    let o = &c.options;
    let e = o.effective();
    let rules = crate::references::path_rules(c);
    let js = o.emit_declaration_only() != Some(true);
    let dts = e.declaration() || o.emit_declaration_only() == Some(true);
    let js_map = o.source_map() == Some(true);
    let dts_map = o.declaration_map() == Some(true);
    let mut out = Vec::new();
    for f in &c.file_names {
        let lower = f.to_ascii_lowercase();
        if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
            continue;
        }
        if lower.ends_with(".json") {
            if js && let Some(j) = rules.json_output(f) {
                out.push(j);
            }
            continue;
        }
        if js && let Some(j) = rules.js_output(f) {
            if js_map {
                out.push(format!("{j}.map"));
            }
            out.push(j);
        }
        if dts && let Some(d) = rules.dts_output(f) {
            if dts_map {
                out.push(format!("{d}.map"));
            }
            out.push(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::buildinfo::{FileState, ProgramState};

    ts_r_testing::cap_memory!();

    #[test]
    fn checks_each_declaration_when_latest_change_matches() {
        let dir = std::env::temp_dir().join(format!(
            "ts-r-projects-declaration-status-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file =
            |name: &str| ts_r_config::path::normalize_slashes(&dir.join(name).to_string_lossy());
        let write = |path: &str, text: &str, seconds: u64| {
            std::fs::write(path, text).unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
        };
        let config_path = file("tsconfig.json");
        let root = file("app.ts");
        let a = file("a.d.ts");
        let b = file("b.d.ts");
        let a_old = "export interface A { x: number }\n";
        let b_old = "export interface B { y: number }\n";
        let root_text = "import { A } from './a'; import { B } from './b';\n";
        let mut fs = ts_r_config::MemoryFs::new();
        fs.add_file(
            &config_path,
            r#"{"compilerOptions":{"composite":true,"noEmit":true},"files":["app.ts"]}"#,
        )
        .add_file(&root, root_text);
        let config = ts_r_config::load_tsconfig(&fs, &config_path);
        assert!(config.diagnostics.is_empty());
        let bi_path = buildinfo::path_for(&config.options, &config_path);
        let info = BuildInfo {
            version: buildinfo::current_version(),
            root: vec![buildinfo::relative_to(&bi_path, &root, true)],
            program: Some(ProgramState {
                options: option_hashes(&config.options, &config.config_dir),
                files: [(&root, root_text), (&a, a_old), (&b, b_old)]
                    .into_iter()
                    .map(|(path, text)| FileState {
                        path: buildinfo::relative_to(&bi_path, path, true),
                        hash: buildinfo::content_hash(path, text.as_bytes()),
                        ..FileState::default()
                    })
                    .collect(),
                ..ProgramState::default()
            }),
            ..BuildInfo::default()
        };
        write(&root, root_text, 100);
        write(&bi_path, &buildinfo::to_text(&info), 200);
        // Both changed upstream, then B alone reverted. Its matching hash must not hide A.
        write(&a, "export interface A { x: string }\n", 300);
        write(&b, b_old, 400);
        let upstream_bi = file("lib.tsbuildinfo");
        let upstream_info = BuildInfo {
            version: buildinfo::current_version(),
            program: Some(ProgramState {
                latest_changed_dts: Some(buildinfo::relative_to(&upstream_bi, &b, true)),
                ..ProgramState::default()
            }),
            ..BuildInfo::default()
        };
        write(&upstream_bi, &buildinfo::to_text(&upstream_info), 500);
        let mut references = ProjectReferences::new(true);
        references.add(&file("a.ts"), &a, None);
        references.add(&file("b.ts"), &b, None);
        for built in [false, true] {
            let upstream = [Upstream {
                shown: file("lib"),
                config: file("lib.json"),
                buildinfo: upstream_bi.clone(),
                errors: false,
                skipped: false,
                built,
            }];
            let inputs = Inputs {
                config: &config,
                upstream: &upstream,
                references: &references,
                force: false,
                stop_on_errors: false,
                watch: false,
            };
            assert_eq!(
                check(&inputs),
                Status::OlderThanInput {
                    output: bi_path.clone(),
                    input: a.clone(),
                }
            );
        }
        // A mapping comment alone still leaves the dependency's declarations unchanged.
        write(
            &a,
            &format!("{a_old}//# sourceMappingURL=a.d.ts.map\n"),
            300,
        );
        let upstream = [Upstream {
            shown: file("lib"),
            config: file("lib.json"),
            buildinfo: upstream_bi,
            errors: false,
            skipped: false,
            built: false,
        }];
        assert_eq!(
            check(&Inputs {
                config: &config,
                upstream: &upstream,
                references: &references,
                force: false,
                stop_on_errors: false,
                watch: false,
            }),
            Status::TouchForUpstream
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
