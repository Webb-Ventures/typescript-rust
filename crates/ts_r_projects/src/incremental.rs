//! Incremental emit (design record 0012, E3): which outputs a build writes and the buildinfo
//! it leaves, given the previous buildinfo, the loaded program and its in-memory emit.
//!
//! The rules, observed in tsc 7.0.2's `tsc/incremental` and `tsbuild` baselines:
//!
//! - A file is *changed* when its content hash differs from the stored one (or it is new).
//!   Changed files are affected and their signature is computed. A file's signature starts
//!   as its version (never computed: the next computation counts as a change). When a
//!   signature changed, the file's direct importers are affected and their signatures
//!   computed in turn, as long as signatures keep changing (not under `isolatedModules`).
//!   Every other transitive importer of a changed signature "may change its declarations":
//!   only its declaration output is pending and its signature is reset to its version,
//!   unless a declaration file is written now (the signature is then the `.d.ts` hash).
//!   Removing a file affects its old importers. A changed file that affects the global
//!   scope (a script, `declare global`) makes tsc re-check everything but re-emit only the
//!   declarations that change, which the byte comparison below covers.
//! - Affected files get their outputs pending: JavaScript and, when declarations are on,
//!   declarations. An emitting build clears what it produced per file; a file with a
//!   declaration emit diagnostic (TS4094 and friends) is not kept pending. Pending outputs are written by the next build that emits that kind
//!   (`noEmit` and `noEmitOnError` with errors keep them pending; the buildinfo records them
//!   and the up-to-date check reports "some of the changes were not emitted").
//! - The options that decide output bytes are hashed per kind (`affects::EMIT` for
//!   JavaScript, `affects::DECL_EMIT` for declarations). A kind emitted with other options
//!   than last time is pending for every file; turning a kind off never re-emits.
//!   `affects::CHECK` options changing only make the up-to-date check rebuild (every
//!   build checks the whole program); they do not re-emit anything.
//! - On top of tsc's set, any output whose bytes differ from the file on disk is written:
//!   whatever the buildinfo says, a build leaves exactly a clean build's outputs.
//! - The buildinfo is written only when its text changes.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_checker::ProgramFiles;
use ts_r_config::options::{OPTIONS, OptionType, affects};
use ts_r_config::{CompilerOptions, OptionId, OptionValue, ParsedConfig, path};
use ts_r_emit::EmitResult;

use crate::buildinfo::{
    self, BuildInfo, FileState, OptionHashes, ProgramState, Read, current_version, hash_hex,
    pending,
};

/// The option-class hashes of `options` (paths relative to `config_dir`).
pub fn option_hashes(options: &CompilerOptions, config_dir: &str) -> OptionHashes {
    let e = options.effective();
    let hash = |include: &dyn Fn(OptionId, u8) -> bool| {
        let mut s = String::new();
        for d in OPTIONS {
            if !include(d.id, d.affects) {
                continue;
            }
            // `declaration` is implied by `composite`: hash what it means.
            let v = if d.id == OptionId::Declaration {
                Some(format!("{}", e.declaration()))
            } else if d.id == OptionId::Strict
                && matches!(options.get(OptionId::Strict), Some(OptionValue::Bool(true)))
            {
                // tsc 7's default: an explicit `strict: true` changes nothing (observed:
                // `tsbuildWatch/programUpdates/works-with-extended-source-files`).
                None
            } else {
                options.get(d.id).map(|v| value_text(v, d.ty, config_dir))
            };
            if let Some(v) = v {
                s.push_str(d.name);
                s.push('=');
                s.push_str(&v);
                s.push('\n');
            }
        }
        hash_hex(s.as_bytes())
    };
    OptionHashes {
        // `noCheck` decides what is reported, not what a check finds: toggling it is not an
        // option change (observed; `check_pending` covers it). Nor is turning `declaration`
        // off: no declaration output is asked for (observed: `tsbuild/commandLine/different-
        // options-with-incremental`); turning it on is a new output kind (`status`).
        check: hash(&|id, a| {
            a & affects::CHECK != 0 && !matches!(id, OptionId::NoCheck | OptionId::Declaration)
        }),
        // A new `declarationDir` re-emits the JavaScript too (observed:
        // `tsbuild/sample/sample`, "when logic config changes declaration dir").
        js: Some(hash(&|id, a| {
            (a & affects::EMIT != 0 && id != OptionId::EmitDeclarationOnly)
                || id == OptionId::DeclarationDir
        })),
        dts: Some(hash(&|id, a| {
            a & affects::DECL_EMIT != 0
                && !matches!(id, OptionId::Declaration | OptionId::Composite)
        })),
        js_maps: Some(
            u8::from(options.source_map() == Some(true))
                | u8::from(options.inline_source_map() == Some(true)) << 1,
        ),
        dts_maps: Some(u8::from(options.declaration_map() == Some(true))),
    }
}

fn value_text(v: &OptionValue, ty: OptionType, config_dir: &str) -> String {
    let rel = |p: &str| path::relative(config_dir, p, true);
    match (v, ty) {
        (OptionValue::String(p), OptionType::Path) => rel(p),
        (OptionValue::List(l), OptionType::PathList) => {
            l.iter().map(|p| rel(p)).collect::<Vec<_>>().join(",")
        }
        _ => format!("{v:?}"),
    }
}

/// Declaration text without its final `//# sourceMappingURL=` line.
fn without_map_url(text: &[u8]) -> &[u8] {
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    if body[start..].starts_with(b"//# sourceMappingURL=") {
        &text[..start]
    } else {
        text
    }
}

/// Which output kinds a build with `options` writes.
pub fn emit_kinds(options: &CompilerOptions) -> u8 {
    if options.effective().no_emit() {
        return 0;
    }
    option_kinds(options)
}

/// The output kinds `options` ask for, `noEmit` aside: what affected files wait for.
pub fn option_kinds(options: &CompilerOptions) -> u8 {
    let e = options.effective();
    let dts_only = options.emit_declaration_only() == Some(true);
    let mut k = 0;
    if !dts_only {
        k |= pending::JS;
    }
    if e.declaration() || dts_only {
        k |= pending::DTS;
    }
    k
}

/// True for `.d.ts`, `.d.mts`, `.d.cts` and their maps: declaration outputs.
pub fn is_declaration_output(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    let l = l.strip_suffix(".map").unwrap_or(&l);
    l.ends_with(".d.ts") || l.ends_with(".d.mts") || l.ends_with(".d.cts")
}

/// A project's previous build, as the next build sees it.
#[derive(Debug, Clone)]
pub struct Previous {
    pub buildinfo_path: String,
    /// The project keeps program state (`incremental`/`composite`).
    pub incremental: bool,
    pub read: Read,
}

impl Previous {
    pub fn load(config: &ParsedConfig) -> Previous {
        let buildinfo_path = buildinfo::path_for(&config.options, &config.config_path);
        let read = buildinfo::read(&buildinfo_path);
        Previous {
            incremental: buildinfo::is_incremental(&config.options),
            buildinfo_path,
            read,
        }
    }

    fn state(&self) -> Option<&ProgramState> {
        match &self.read {
            Read::Ours(b) => b.program.as_ref(),
            _ => None,
        }
    }
}

/// What a build writes.
#[derive(Debug, Default)]
pub struct Plan {
    /// Indices into `EmitResult::outputs` to write, in order.
    pub write: Vec<usize>,
    pub buildinfo: BuildInfo,
    /// The buildinfo text, `None` when it equals the file on disk.
    pub buildinfo_text: Option<String>,
}

/// What the build produced, for [`plan`].
#[derive(Debug)]
pub struct Built<'a> {
    pub config: &'a ParsedConfig,
    /// `None` when the program could not be loaded (option errors stop before it).
    pub program: Option<&'a ProgramFiles>,
    /// The in-memory emit (`None`: nothing emitted, `noEmit`).
    pub emitted: Option<&'a EmitResult>,
    /// `noEmitOnError` with errors: nothing is written.
    pub blocked: bool,
    /// The build reported errors other than semantic ones.
    pub errors: bool,
    /// The build reported semantic (checker) errors.
    pub semantic_errors: bool,
    /// The build ran with `noCheck` (no semantic diagnostics).
    pub no_check: bool,
    /// Always write every output and the buildinfo (`--build` of a project without
    /// program state: tsc rewrites everything).
    pub write_all: bool,
    /// Write a buildinfo (`--build`, or `incremental`/`composite`).
    pub with_buildinfo: bool,
    /// The `package.json` paths resolution looked for
    /// (`ts_r_checker::program::load_program_tracking_package_jsons`); `None` keeps the
    /// previous buildinfo's lists.
    pub package_jsons: Option<&'a [String]>,
}

/// Decides the writes and the new buildinfo. `on_disk` reads an existing output.
pub fn plan(prev: &Previous, b: &Built, on_disk: &dyn Fn(&str) -> Option<Vec<u8>>) -> Plan {
    let cs = b.config.case_sensitive();
    let bi_path = prev.buildinfo_path.as_str();
    let rel = |p: &str| buildinfo::relative_to(bi_path, p, cs);
    let root: Vec<String> = b.config.file_names.iter().map(|f| rel(f)).collect();
    let kinds = emit_kinds(&b.config.options);
    let writing = b.emitted.is_some() && !b.blocked;
    let mut write = Vec::new();
    let mut info = BuildInfo {
        version: current_version(),
        root,
        errors: b.errors,
        semantic_errors: b.semantic_errors,
        check_pending: false,
        program: None,
    };

    // Composite projects track their declaration text and leave an unchanged `.d.ts`
    // alone; other incremental projects rewrite it with the same content (both observed).
    let composite = b.config.options.composite() == Some(true);
    match (prev.incremental, b.program) {
        (true, Some(program)) => {
            let (state, pending_writes) = program_state(prev, b, program, kinds, writing);
            if let Some(result) = b.emitted.filter(|_| writing) {
                for (i, o) in result.outputs.iter().enumerate() {
                    let kind = if is_declaration_output(&o.path) {
                        pending::DTS
                    } else {
                        pending::JS
                    };
                    let due = pending_writes.get(&o.source).is_some_and(|k| k & kind != 0);
                    let same = on_disk(&o.path).as_deref() == Some(o.text.as_bytes());
                    // A declaration output whose text did not change is not rewritten, even
                    // when its file is re-emitted (observed: tsc 7 leaves such `.d.ts` files
                    // untouched, so downstream projects only update timestamps). Its map is
                    // rewritten all the same (observed: `*rewrite with same content*`).
                    let is_map = o.path.to_ascii_lowercase().ends_with(".map");
                    // Without a previous state (no buildinfo, or another version's) the old
                    // declaration text is not known: everything is rewritten (observed:
                    // `rebuilds-completely-when-version-in-tsbuildinfo-doesnt-match`).
                    let known = prev.state().is_some();
                    let due =
                        due && !(kind == pending::DTS && same && composite && known && !is_map);
                    if b.write_all || due || !same {
                        write.push(i);
                    }
                }
            }
            let mut state = state;
            // The last declaration output whose content changes.
            if let Some(result) = b.emitted.filter(|_| writing) {
                for &i in &write {
                    let o = &result.outputs[i];
                    let l = o.path.to_ascii_lowercase();
                    // A `sourceMappingURL` comment added or dropped (`declarationMap`
                    // toggled) is not a change (observed: downstream projects stay up to
                    // date with the dependency's `.d.ts` files).
                    if is_declaration_output(&o.path)
                        && !l.ends_with(".map")
                        && on_disk(&o.path).as_deref().is_none_or(|d| {
                            without_map_url(d) != without_map_url(o.text.as_bytes())
                        })
                    {
                        state.latest_changed_dts = Some(rel(&o.path));
                    }
                }
            }
            info.program = Some(state);
        }
        (true, None) => {
            // Nothing loaded (option errors): keep what we had, so nothing is lost.
            info.program = prev.state().cloned();
        }
        (false, _) => {
            if let Some(result) = b.emitted.filter(|_| writing) {
                write.extend(0..result.outputs.len());
            }
        }
    }

    // `noCheck`: files are left unchecked unless nothing changed since a build that left
    // none, and what that build found stays (observed: a no-change `noCheck` build after a
    // checking one keeps the buildinfo, semantic errors included).
    if b.no_check {
        let prev_info = match &prev.read {
            Read::Ours(p) => Some(p.as_ref()),
            _ => None,
        };
        let unchanged = match (prev_info, info.program.as_ref()) {
            (Some(p), Some(now)) => p.program.as_ref().is_some_and(|old| {
                old.options.check == now.options.check
                    && old.files.len() == now.files.len()
                    && old
                        .files
                        .iter()
                        .zip(&now.files)
                        .all(|(a, b)| a.path == b.path && a.hash == b.hash)
            }),
            _ => false,
        };
        match prev_info.filter(|_| unchanged) {
            Some(p) => {
                info.check_pending = p.check_pending;
                info.semantic_errors = p.semantic_errors;
            }
            None => info.check_pending = true,
        }
    }
    let text = buildinfo::to_text(&info);
    let unchanged = !b.write_all && on_disk(bi_path).is_some_and(|old| old == text.as_bytes());
    Plan {
        write,
        buildinfo_text: (b.with_buildinfo && !unchanged).then_some(text),
        buildinfo: info,
    }
}

/// The new program state and, per program file index, the output kinds due now.
fn program_state(
    prev: &Previous,
    b: &Built,
    program: &ProgramFiles,
    kinds: u8,
    writing: bool,
) -> (ProgramState, FxHashMap<usize, u8>) {
    let cs = b.config.case_sensitive();
    let bi_path = prev.buildinfo_path.as_str();
    let hashes = option_hashes(&b.config.options, &b.config.config_dir);
    // A change of checking options re-checks (every build checks everything) but does not
    // re-emit: `declaration` turned on writes only declarations (observed).
    let old = prev.state();

    // Program files without libraries; `ids[program index]` = buildinfo index.
    let mut ids: Vec<Option<u32>> = vec![None; program.files.len()];
    let mut files: Vec<FileState> = Vec::new();
    let mut emittable: Vec<bool> = Vec::new();
    let mut program_index: Vec<usize> = Vec::new();
    let mut by_path: FxHashMap<String, u32> = FxHashMap::default();
    let old_files: FxHashMap<&str, &FileState> = old
        .map(|o| o.files.iter().map(|f| (f.path.as_str(), f)).collect())
        .unwrap_or_default();
    for (pi, f) in program.files.iter().enumerate() {
        if f.is_lib {
            continue;
        }
        let id = files.len() as u32;
        ids[pi] = Some(id);
        by_path.insert(path::key(&f.path, cs), id);
        let rel = buildinfo::relative_to(bi_path, &f.path, cs);
        let hash = buildinfo::content_hash(&f.path, f.text.as_bytes());
        // Signatures are computed below, for the files whose shape matters; the others keep
        // theirs (a new file starts as its version).
        let (signature, api) = match old_files.get(rel.as_str()) {
            Some(o) => (o.signature.clone(), o.api.clone()),
            None => (String::new(), String::new()),
        };
        files.push(FileState {
            path: rel,
            hash,
            signature,
            api,
            global: !f.bound.is_module || !f.bound.table(f.bound.global_augmentations).is_empty(),
            deps: Vec::new(),
        });
        emittable.push(ts_r_emit::emittable(f));
        program_index.push(pi);
    }
    for (id, &pi) in program_index.iter().enumerate() {
        let f = &program.files[pi];
        if let Some(deps) = program.deps.get(&f.path) {
            let mut d: Vec<u32> = deps
                .iter()
                .filter_map(|p| by_path.get(&path::key(p, cs)).copied())
                .filter(|&x| x as usize != id)
                .collect();
            d.sort_unstable();
            d.dedup();
            files[id].deps = d;
        }
    }

    let n = files.len();
    // `affected`: the file's outputs are pending (changed files, and importers of a file
    // whose signature changed, as long as signatures keep changing). `dts_may_change`: every
    // importer, transitively, of a file whose signature changed; only declarations wait
    // (observed in `tsc/noEmit/changes-*`: tsc 7 re-checks those, keeps their
    // JavaScript, and its signature becomes the file's version again).
    let mut affected = vec![false; n];
    let mut dts_may_change = vec![false; n];
    let mut js_may_change = vec![false; n];
    let mut pending_bits = vec![0u8; n];
    // Library files: names and contents. Another set (`noLib`, `lib`, `target` toggled, a
    // lib edited) affects every file, as a changed global file does.
    let libs = {
        let mut s = String::new();
        for f in program.files.iter().filter(|f| f.is_lib) {
            s.push_str(path::file_name(&f.path));
            s.push('\0');
            s.push_str(&buildinfo::content_hash(&f.path, f.text.as_bytes()));
            s.push('\n');
        }
        hash_hex(s.as_bytes())
    };
    let libs_changed = old.is_some_and(|o| !o.libs.is_empty() && o.libs != libs);
    match old {
        None => affected.iter_mut().for_each(|a| *a = true),
        Some(old) => {
            let old_by_path: FxHashMap<&str, usize> = old
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| (f.path.as_str(), i))
                .collect();
            // Importers, for propagation.
            let mut importers: Vec<Vec<u32>> = vec![Vec::new(); n];
            for (i, f) in files.iter().enumerate() {
                for &d in &f.deps {
                    importers[d as usize].push(i as u32);
                }
            }
            // Computes file `i`'s signature now and tells whether it changed.
            let evaluate = |i: usize, files: &mut [FileState]| -> bool {
                let pi = program_index[i];
                let dts = declaration_signature(b.emitted, pi);
                let api = api_signature(&program.files[pi]);
                let old_hash = old_by_path
                    .get(files[i].path.as_str())
                    .map(|&j| old.files[j].hash.as_str());
                let f = &mut files[i];
                let changed = if f.signature.is_empty() && f.api.is_empty() {
                    true
                } else if let (Some(d), false) = (&dts, f.signature.is_empty()) {
                    *d != f.signature
                } else if !f.api.is_empty() {
                    api != f.api
                } else {
                    // Only a declaration signature is known and none was emitted now: the
                    // text decides.
                    old_hash != Some(f.hash.as_str())
                };
                f.signature = dts.unwrap_or_default();
                f.api = api;
                changed
            };
            let mut queue: Vec<u32> = Vec::new();
            for i in 0..n {
                match old_by_path
                    .get(files[i].path.as_str())
                    .map(|&j| &old.files[j])
                {
                    Some(o) if o.hash == files[i].hash => {
                        if libs_changed {
                            affected[i] = true;
                            evaluate(i, &mut files);
                        }
                    }
                    Some(_) => {
                        affected[i] = true;
                        if evaluate(i, &mut files) {
                            queue.push(i as u32);
                        }
                    }
                    None => {
                        affected[i] = true;
                        evaluate(i, &mut files);
                        queue.push(i as u32);
                    }
                }
            }
            // Removed files: their old importers are affected.
            let current: FxHashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
            let mut removed_importers = Vec::new();
            for (j, o) in old.files.iter().enumerate() {
                if current.contains(o.path.as_str()) {
                    continue;
                }
                for (k, other) in old.files.iter().enumerate() {
                    if other.deps.contains(&(j as u32))
                        && let Some(&i) = by_path.get(&path::key(
                            &buildinfo::absolute(bi_path, &old.files[k].path),
                            cs,
                        ))
                    {
                        removed_importers.push(i);
                    }
                }
            }
            for i in removed_importers {
                if !affected[i as usize] {
                    affected[i as usize] = true;
                    evaluate(i as usize, &mut files);
                }
                queue.push(i);
            }
            // Importers of a file whose signature changed are affected; theirs is computed
            // and, when it changed too, their importers are next. Under `isolatedModules`
            // no other file's JavaScript can change: importers are only `dts_may_change`
            // (observed: `tsbuild/inferredTypeFromTransitiveModule/*-with-isolatedModules`).
            let isolated = b.config.options.effective().isolated_modules();
            let mut changed_shape: Vec<u32> = Vec::new();
            let mut seen = vec![false; n];
            while let Some(i) = queue.pop() {
                if std::mem::replace(&mut seen[i as usize], true) {
                    continue;
                }
                changed_shape.push(i);
                if isolated {
                    continue;
                }
                for &imp in &importers[i as usize] {
                    if affected[imp as usize] {
                        continue;
                    }
                    affected[imp as usize] = true;
                    if evaluate(imp as usize, &mut files) {
                        queue.push(imp);
                    }
                }
            }
            // A changed file declaring a const enum: its values are inlined in the
            // JavaScript of every transitive importer, which is re-emitted too (observed:
            // `tsc/incremental/const-enums*`).
            let const_enum_roots: Vec<u32> = changed_shape
                .iter()
                .copied()
                .filter(|&i| declares_const_enum(&program.files[program_index[i as usize]].ast))
                .collect();
            let mut stack = changed_shape;
            let mut walked = vec![false; n];
            while let Some(i) = stack.pop() {
                for &imp in &importers[i as usize] {
                    if !std::mem::replace(&mut walked[imp as usize], true) {
                        dts_may_change[imp as usize] = !affected[imp as usize];
                        stack.push(imp);
                    }
                }
            }
            let mut stack = const_enum_roots;
            let mut walked = vec![false; n];
            while let Some(i) = stack.pop() {
                for &imp in &importers[i as usize] {
                    if !std::mem::replace(&mut walked[imp as usize], true) {
                        js_may_change[imp as usize] = dts_may_change[imp as usize];
                        stack.push(imp);
                    }
                }
            }
            for (i, f) in files.iter_mut().enumerate() {
                if dts_may_change[i] {
                    f.signature.clear();
                    f.api.clear();
                }
            }
            for &(i, bits) in &old.pending {
                if let Some(f) = old.files.get(i as usize)
                    && let Some(&j) =
                        by_path.get(&path::key(&buildinfo::absolute(bi_path, &f.path), cs))
                {
                    pending_bits[j as usize] |= bits;
                }
            }
        }
    }
    // A declaration output written now is the signature (tsc's "stored at emit"). The API
    // hash goes with it: a later build without declarations compares that instead (tsc
    // still computes the `.d.ts` signature then; observed: `tsbuild/commandLine/different-
    // options-with-incremental`, a local change rewrites no importer).
    if writing {
        for (i, f) in files.iter_mut().enumerate() {
            if let Some(d) = declaration_signature(b.emitted, program_index[i]) {
                f.signature = d;
                if f.api.is_empty() {
                    f.api = api_signature(&program.files[program_index[i]]);
                }
            }
        }
    }
    let declarations = b.config.options.effective().declaration()
        || b.config.options.emit_declaration_only() == Some(true);
    // Only the kinds these options produce: JavaScript under `emitDeclarationOnly` is not
    // waiting, its later emit is a change of options (observed: "there is change in
    // compilerOptions").
    let all_kinds = option_kinds(&b.config.options);
    for i in 0..n {
        // Importers of a changed const enum re-emit their JavaScript too (inlined values).
        if affected[i] || js_may_change[i] {
            pending_bits[i] |= all_kinds;
        } else if dts_may_change[i] {
            pending_bits[i] |= all_kinds & pending::DTS;
        }
    }
    // A kind emitted now with other options than last time: every file.
    let old_hashes = prev.state().map(|s| &s.options);
    let mut stored = OptionHashes {
        check: hashes.check.clone(),
        js: old_hashes.and_then(|h| h.js.clone()),
        dts: old_hashes.and_then(|h| h.dts.clone()),
        js_maps: old_hashes.and_then(|h| h.js_maps),
        dts_maps: old_hashes.and_then(|h| h.dts_maps),
    };
    if kinds & pending::JS != 0 && stored.js != hashes.js {
        pending_bits.iter_mut().for_each(|p| *p |= pending::JS);
    }
    if kinds & pending::DTS != 0 && stored.dts != hashes.dts {
        pending_bits.iter_mut().for_each(|p| *p |= pending::DTS);
    }
    for (i, p) in pending_bits.iter_mut().enumerate() {
        if !emittable[i] {
            *p = 0;
        } else if !declarations {
            *p &= !pending::DTS;
        }
    }
    let mut due: FxHashMap<usize, u8> = FxHashMap::default();
    let skipped = b.emitted.is_some_and(|r| r.skipped);
    if writing {
        // When something was not written, a file's kind stays pending unless it was
        // produced, or tsc's own emit diagnostics explain why not (TS4094 and the like:
        // tsc keeps those as emit diagnostics, nothing pending; observed:
        // `tsc/noCheck/dts-errors-with-incremental`). ts-r's refusals (TSR codes) wait.
        let mut produced: FxHashMap<usize, u8> = FxHashMap::default();
        let mut explained: FxHashSet<usize> = FxHashSet::default();
        if let Some(r) = b.emitted.filter(|_| skipped) {
            for o in &r.outputs {
                let kind = if is_declaration_output(&o.path) {
                    pending::DTS
                } else {
                    pending::JS
                };
                *produced.entry(o.source).or_default() |= kind;
            }
            explained.extend(
                r.diagnostics
                    .iter()
                    .filter(|d| d.code < ts_r_diagnostics::SOUND_CODE_BASE)
                    .map(|d| d.file.0 as usize),
            );
        }
        for (i, p) in pending_bits.iter_mut().enumerate() {
            let pi = program_index[i];
            let now = *p & kinds;
            if now != 0 {
                due.insert(pi, now);
            }
            let done = if !skipped || explained.contains(&pi) {
                kinds
            } else {
                produced.get(&pi).copied().unwrap_or(0) & kinds
            };
            *p &= !done;
        }
        // A kind this build does not produce is forgotten once something is emitted:
        // emitting it again later is a change of options that re-emits every file (observed
        // with `emitDeclarationOnly` toggled on the command line). A build that emits
        // nothing leaves the stored options alone (tsc does not rewrite its buildinfo then).
        let emits = !due.is_empty();
        if kinds & pending::JS != 0 {
            stored.js = hashes.js.clone();
            stored.js_maps = hashes.js_maps;
        } else if emits {
            stored.js = None;
            stored.js_maps = None;
        }
        if kinds & pending::DTS != 0 {
            // Files whose declarations tsc's emit diagnostics explain are not waiting: the
            // options are stored as with a complete emit (observed: `tsc/noCheck/dts-errors-
            // with-incremental`, a rebuild writes nothing).
            if !skipped || pending_bits.iter().all(|p| p & pending::DTS == 0) {
                stored.dts = hashes.dts.clone();
                stored.dts_maps = hashes.dts_maps;
            }
        } else if emits {
            stored.dts = None;
            stored.dts_maps = None;
        }
    }
    let pending: Vec<(u32, u8)> = pending_bits
        .iter()
        .enumerate()
        .filter(|(_, p)| **p != 0)
        .map(|(i, p)| (i as u32, *p))
        .collect();
    let _ = ids;
    (
        ProgramState {
            options: stored,
            files,
            pending,
            latest_changed_dts: old.and_then(|s| s.latest_changed_dts.clone()),
            libs,
            package_jsons: match b.package_jsons {
                Some(l) => l
                    .iter()
                    .filter(|p| std::path::Path::new(p).is_file())
                    .map(|p| buildinfo::relative_to(bi_path, p, cs))
                    .collect(),
                None => old.map(|s| s.package_jsons.clone()).unwrap_or_default(),
            },
            missing_package_jsons: match b.package_jsons {
                Some(l) => l
                    .iter()
                    .filter(|p| !std::path::Path::new(p).is_file())
                    .map(|p| buildinfo::relative_to(bi_path, p, cs))
                    .collect(),
                None => old
                    .map(|s| s.missing_package_jsons.clone())
                    .unwrap_or_default(),
            },
        },
        due,
    )
}

/// The hash of the declaration output emitted for program file `pi`, when there is one (the
/// E2 seam: with `.d.ts` emit the signature is tsc's, the text of the declaration file).
fn declaration_signature(emitted: Option<&EmitResult>, pi: usize) -> Option<String> {
    let r = emitted?;
    r.outputs
        .iter()
        .find(|o| {
            o.source == pi
                && is_declaration_output(&o.path)
                && !o.path.to_ascii_lowercase().ends_with(".map")
        })
        .map(|o| hash_hex(o.text.as_bytes()))
}

/// The signature of a file without a declaration output: declaration files hash their text;
/// others the declared API (`ts_r_decls`), which changes whenever the `.d.ts` would.
fn api_signature(f: &ts_r_checker::SourceFile) -> String {
    if f.is_declaration() {
        return buildinfo::content_hash(&f.path, f.text.as_bytes());
    }
    let idx = ts_r_decls::index(&f.ast, &f.bound);
    let mut s = String::with_capacity(32);
    for b in &idx.api_hash[..16] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The file declares a const enum (anywhere: namespaces too).
fn declares_const_enum(ast: &ts_r_ast::Ast) -> bool {
    (0..ast.node_count() as u32).any(|raw| {
        ts_r_ast::NodeId::from_raw(raw).is_some_and(|id| {
            matches!(ast.node(id), ts_r_ast::Node::Enum { modifiers, .. }
                if modifiers.contains(ts_r_ast::Modifiers::CONST))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_outputs() {
        assert!(is_declaration_output("/p/a.d.ts"));
        assert!(is_declaration_output("/p/a.d.ts.map"));
        assert!(is_declaration_output("/p/a.d.mts"));
        assert!(!is_declaration_output("/p/a.js"));
        assert!(!is_declaration_output("/p/a.js.map"));
    }

    #[test]
    fn option_classes() {
        let mut a = CompilerOptions::new();
        a.set(OptionId::Composite, Some(OptionValue::Bool(true)), None);
        let mut b = a.clone();
        b.set(OptionId::Declaration, Some(OptionValue::Bool(true)), None);
        // `declaration` under `composite` changes nothing.
        assert_eq!(option_hashes(&a, "/p"), option_hashes(&b, "/p"));
        let mut c = a.clone();
        c.set(OptionId::SourceMap, Some(OptionValue::Bool(true)), None);
        let (ha, hc) = (option_hashes(&a, "/p"), option_hashes(&c, "/p"));
        assert_eq!(ha.check, hc.check);
        assert_eq!(ha.dts, hc.dts);
        assert_ne!(ha.js, hc.js);
        let mut d = a.clone();
        d.set(
            OptionId::EmitDeclarationOnly,
            Some(OptionValue::Bool(true)),
            None,
        );
        assert_eq!(option_hashes(&a, "/p"), option_hashes(&d, "/p"));
    }
}
