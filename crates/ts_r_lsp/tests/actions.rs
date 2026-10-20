//! Fixture tests of the ts-r backend's code actions (L4): organize imports, quick fixes,
//! refactors and file renames, run in-process on our checker, compared with tsserver's
//! output on the same input.
//!
//! Each `tests/fixtures/actions/<name>.txt` holds directives, then files:
//!
//! ```text
//! // @action: organizeImports [RemoveUnused|SortAndCombine]
//! //          fix <code> <title>        (the quick fix for the diagnostic at the cursor)
//! //          fixAll <code> <title>     (the fix-all of that quick fix)
//! //          refactor <name>/<action>  (tsserver's refactor and action names)
//! //          rename <from> <to>        (workspace/willRenameFiles)
//! //          moveSuggestions           (ts-r/moveToFileSuggestions at the selection)
//! // @tsconfig: {...}                   (default: strict, esnext, bundler, react-jsx)
//! // @trigger: implicit                 (refactors asked on a cursor move; default: invoked)
//! // @target: <file>                    (Move to file: the chosen target file)
//! // @format: {...}                     (tsserver format options, e.g. indentSize)
//! // @preferences: {...}                (tsserver user preferences, oracle side)
//! // @open: <file>, ...                 (rename: only these are open; default every source)
//! // @symlinks: <link> -> <target>, ... (workspace packages in node_modules)
//! // @differs: <why ts-r's output differs from tsserver's>
//! // @file: src/main.ts
//! ...text with a cursor /*|*/ or a selection /*[*/ ... /*]*/...
//! ```
//!
//! `<name>.tsserver` is tsserver's result (`tests/oracle/actions.js`); our result must equal
//! it, or, for a fixture with `@differs`, equal `<name>.expected` (written when
//! `TS_R_UPDATE_SNAPSHOTS=1`). Results list the changed files in full (`// @file:`).
//! Skipped when the lib files are missing.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use ts_r_checker::{Checker, Ck, ProgramFiles, load_program};
use ts_r_config::{FileSystem, MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;
use ts_r_lsp::text::{Pos, TextDoc};
use ts_r_lsp::tsr::convert::{DiagKind, LspLines, diagnostic_to_lsp};
use ts_r_lsp::tsr::features::Ctx;
use ts_r_lsp::tsr::features::actions;

ts_r_testing::cap_memory!();

const ROOT: &str = "/proj";
const DEFAULT_TSCONFIG: &str = r#"{"compilerOptions": {"strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler", "noEmit": true, "jsx": "react-jsx"}}"#;

#[derive(Debug, Default)]
struct Fixture {
    dir: BTreeMap<String, String>,
    files: Vec<(String, String)>,
    /// File, start, end (byte offsets in that file's text).
    sel: Option<(String, usize, usize)>,
}

fn parse_fixture(text: &str) -> Fixture {
    let mut fx = Fixture::default();
    let mut cur: Option<usize> = None;
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix("// @") {
            let (k, v) = rest.split_once(':').unwrap_or((rest, ""));
            let v = v.strip_prefix(' ').unwrap_or(v);
            if k == "file" {
                fx.files.push((v.trim().to_string(), String::new()));
                cur = Some(fx.files.len() - 1);
                continue;
            }
            if cur.is_none() {
                fx.dir.insert(k.to_string(), v.to_string());
                continue;
            }
        }
        if let Some(i) = cur {
            fx.files[i].1.push_str(line);
            fx.files[i].1.push('\n');
        }
    }
    for (_, t) in &mut fx.files {
        if t.ends_with("\n\n") {
            t.pop();
        }
    }
    for (name, t) in &mut fx.files {
        if let Some(a) = t.find("/*[*/") {
            t.replace_range(a..a + 5, "");
            let b = t.find("/*]*/").expect("selection end");
            t.replace_range(b..b + 5, "");
            fx.sel = Some((name.clone(), a, b));
        }
        if let Some(c) = t.find("/*|*/") {
            t.replace_range(c..c + 5, "");
            fx.sel = Some((name.clone(), c, c));
        }
    }
    fx
}

fn abs(name: &str) -> String {
    format!("{ROOT}/{name}")
}

/// Applies an LSP `WorkspaceEdit` to `texts` (paths relative to the root).
fn apply_workspace_edit(texts: &mut BTreeMap<String, String>, edit: &Value) {
    let rel = |uri: &str| -> String {
        let p = ts_r_lsp::uri::uri_to_path(uri).expect("file uri");
        p.strip_prefix(&format!("{ROOT}/"))
            .unwrap_or(&p)
            .to_string()
    };
    let apply = |texts: &mut BTreeMap<String, String>, uri: &str, edits: &Value| {
        let name = rel(uri);
        let mut doc = TextDoc::new(texts.get(&name).cloned().unwrap_or_default());
        let mut list: Vec<&Value> = edits.as_array().expect("edits").iter().collect();
        // Last first, so earlier positions stay valid.
        list.sort_by_key(|e| {
            let s = Pos::from_lsp(&e["range"]["start"]).expect("start");
            std::cmp::Reverse((s.line, s.character))
        });
        for e in list {
            let r = ts_r_lsp::text::lsp_range(&e["range"]).expect("range");
            doc.apply_change(Some(r), e["newText"].as_str().expect("newText"));
        }
        texts.insert(name, doc.text().to_string());
    };
    if let Some(ch) = edit["changes"].as_object() {
        for (uri, edits) in ch {
            apply(texts, uri, edits);
        }
    }
    for dc in edit["documentChanges"].as_array().into_iter().flatten() {
        match dc["kind"].as_str() {
            Some("create") => {
                texts.entry(rel(dc["uri"].as_str().unwrap())).or_default();
            }
            Some("rename") => {
                let from = rel(dc["oldUri"].as_str().unwrap());
                let to = rel(dc["newUri"].as_str().unwrap());
                if let Some(t) = texts.remove(&from) {
                    texts.insert(to, t);
                }
            }
            Some("delete") => {
                texts.remove(&rel(dc["uri"].as_str().unwrap()));
            }
            _ => apply(
                texts,
                dc["textDocument"]["uri"].as_str().unwrap(),
                &dc["edits"],
            ),
        }
    }
}

fn render(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for n in before.keys().filter(|n| !after.contains_key(*n)) {
        out.push_str(&format!("// @deleted: {n}\n"));
    }
    for (n, t) in after {
        if before.get(n) != Some(t) {
            out.push_str(&format!("// @file: {n}\n{t}"));
        }
    }
    if out.is_empty() {
        return "// no change\n".into();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

struct Built {
    program: ProgramFiles,
}

/// The fixture's files (and its default tsconfig, and `@symlinks`) in memory.
fn fixture_fs(fx: &Fixture, texts: &BTreeMap<String, String>) -> MemoryFs {
    let mut fs = MemoryFs::new();
    if !texts.keys().any(|n| is_config(n)) {
        let cfg = fx
            .dir
            .get("tsconfig")
            .map_or(DEFAULT_TSCONFIG, String::as_str);
        fs.add_file(&abs("tsconfig.json"), cfg);
    }
    for (n, t) in texts {
        fs.add_file(&abs(n), t);
    }
    // `@symlinks: <link> -> <target>, ...` (workspace packages in node_modules).
    for pair in fx.dir.get("symlinks").map_or("", String::as_str).split(',') {
        if let Some((l, t)) = pair.split_once("->") {
            fs.add_symlink(&abs(l.trim()), &abs(t.trim()));
        }
    }
    fs
}

fn is_config(name: &str) -> bool {
    name == "tsconfig.json" || name.ends_with("/tsconfig.json")
}

fn build(fx: &Fixture, texts: &BTreeMap<String, String>) -> Built {
    let libs = ts_r_lsp::tsr::libs().expect("libs");
    let fs = fixture_fs(fx, texts);
    let parsed = load_tsconfig(&fs, &abs("tsconfig.json"));
    let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 1);
    let _ = fs.file_exists("/");
    Built { program }
}

/// `workspace/willRenameFiles` as the coordinator answers it: each loaded project (a
/// tsconfig of the fixture whose files include an open file; `@open`, default every source
/// file) computes its edits and those of its tsconfig, and the parts are merged.
fn rename_edit(
    fx: &Fixture,
    texts: &BTreeMap<String, String>,
    open: &[String],
    renames: &[(String, String)],
) -> Value {
    let libs = ts_r_lsp::tsr::libs().expect("libs");
    let fs = fixture_fs(fx, texts);
    let mut configs: Vec<&str> = texts
        .keys()
        .filter(|n| is_config(n))
        .map(String::as_str)
        .collect();
    if configs.is_empty() {
        configs.push("tsconfig.json");
    }
    let mut parts = Vec::new();
    let mut config_parts = Vec::new();
    for c in configs {
        let parsed = load_tsconfig(&fs, &abs(c));
        if !parsed.file_names.iter().any(|f| open.contains(f)) {
            continue;
        }
        let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 1);
        let checker = Checker::new(&program);
        parts.push(actions::rename::will_rename_files(
            &program, &checker, &fs, renames,
        ));
        if let Some(t) = texts.get(c)
            && let Some(e) = actions::rename::config_workspace_edit(&abs(c), t, renames)
        {
            config_parts.push(e);
        }
    }
    parts.sort_by_cached_key(|v| v.to_string());
    parts.extend(config_parts);
    actions::rename::merge(parts)
}

/// A `rename <from> <to>` fixture: the edits, applied, then the files moved; the edits must
/// be the same when asked after the move.
fn run_rename(fx: &Fixture, arg: &str) -> String {
    let (from, to) = arg.split_once(' ').expect("rename <from> <to>");
    let before: BTreeMap<String, String> = fx.files.iter().cloned().collect();
    let renames = [(abs(from), abs(to))];
    let open: Vec<String> = match fx.dir.get("open") {
        Some(o) => o.split_whitespace().map(abs).collect(),
        None => before
            .keys()
            .filter(|n| !n.ends_with(".json"))
            .map(|n| abs(n))
            .collect(),
    };
    let mut texts = before.clone();
    let edit = rename_edit(fx, &texts, &open, &renames);
    // The client moves the files after applying the edit.
    apply_workspace_edit(&mut texts, &edit);
    let texts = move_paths(&texts, from, to);
    // The same edits when asked after the move (tsserver's `getEditsForFileRename` after VS
    // Code moved the files, on a program that already sees them moved). Not when the
    // tsconfig changes: before its edit, the moved files may be outside the project.
    let touches_config = edit["changes"]
        .as_object()
        .is_some_and(|c| c.keys().any(|u| u.ends_with(".json")));
    if !touches_config {
        let mut after = move_paths(&before, from, to);
        let open_after: Vec<String> = move_paths(
            &open.iter().map(|o| (o.clone(), String::new())).collect(),
            &abs(from),
            &abs(to),
        )
        .into_keys()
        .collect();
        let edit2 = rename_edit(fx, &after, &open_after, &renames);
        apply_workspace_edit(&mut after, &edit2);
        if after != texts {
            let mut moved_before = texts.clone();
            moved_before.retain(|k, v| after.get(k) != Some(v));
            return format!(
                "// asked after the move, the edits differ:\n{}",
                render(&moved_before, &after)
            );
        }
    }
    render(&before, &texts)
}

/// Runs one fixture through our code actions: the rendered result.
fn run(fx: &Fixture) -> String {
    let action = fx.dir.get("action").map_or("", |s| s.trim());
    if let Some(arg) = action.strip_prefix("rename ") {
        return run_rename(fx, arg);
    }
    let mut texts: BTreeMap<String, String> = fx.files.iter().cloned().collect();
    let before = texts.clone();
    let built = build(fx, &texts);
    let program = &built.program;
    let checker = Checker::new(program);
    let main = fx
        .sel
        .as_ref()
        .map(|s| s.0.clone())
        .unwrap_or_else(|| fx.files[0].0.clone());
    let Some(fi) = program.file_index(&abs(&main)) else {
        return format!("// {main} is not in the program\n");
    };
    let file = FileId(fi as u32);
    let mut ck = Ck::new(&checker, file);
    let diags = ck.check_source_file(file);
    let text = checker.file(file).text.as_str();
    let lines = LspLines::new(text);
    let (s, e) = fx.sel.as_ref().map_or((0, 0), |s| (s.1 as u32, s.2 as u32));
    let uri = ts_r_lsp::uri::path_to_uri(&abs(&main));
    let action = fx.dir.get("action").map_or("", |s| s.trim());
    let (verb, arg) = action.split_once(' ').unwrap_or((action, ""));
    let cx = Ctx {
        program,
        checker: &checker,
        file,
    };
    let trigger = if fx
        .dir
        .get("trigger")
        .is_some_and(|t| t.trim() == "implicit")
    {
        2
    } else {
        1
    };
    let target = fx.dir.get("target").map(|t| abs(t.trim()));
    let format: Value = fx
        .dir
        .get("format")
        .and_then(|f| serde_json::from_str(f).ok())
        .unwrap_or(Value::Null);
    let params = |only: Vec<&str>, diags: Vec<Value>| {
        json!({
            "textDocument": {"uri": uri},
            "range": lines.range(text, s, e),
            "context": {
                "diagnostics": diags,
                "only": if only.is_empty() { Value::Null } else { json!(only) },
                "triggerKind": trigger,
                "tsr": {"targetFile": target, "format": format},
            },
        })
    };
    let no_rel = |_: &ts_r_diagnostics::RelatedInfo| None;
    let mut note = String::new();
    let chosen: Option<Value> = match verb {
        "organizeImports" => {
            let kind = match arg {
                "RemoveUnused" => "source.removeUnusedImports",
                "SortAndCombine" => "source.sortImports",
                _ => "source.organizeImports",
            };
            let r = actions::code_actions(&cx, &mut ck, &diags, &params(vec![kind], vec![]));
            r.as_array().and_then(|a| a.first().cloned())
        }
        "fix" | "fixAll" => {
            let (code, title) = arg.split_once(' ').unwrap_or((arg, ""));
            let code: u32 = code.parse().expect("code");
            let d = diags
                .iter()
                .find(|d| d.code == code && d.span.start <= s && s <= d.span.end);
            match d {
                None => {
                    let found: Vec<String> = diags
                        .iter()
                        .chain(program.files[fi].syntax.iter())
                        .map(|d| format!("{}@{}", d.code, d.span.start))
                        .collect();
                    note = format!(
                        "// no diagnostic {code} at the cursor; the file has: {}\n",
                        found.join(" ")
                    );
                    None
                }
                Some(d) => {
                    let ld = diagnostic_to_lsp(d, text, &lines, DiagKind::Semantic, &no_rel);
                    let r = actions::code_actions(&cx, &mut ck, &diags, &params(vec![], vec![ld]));
                    let all = r.as_array().cloned().unwrap_or_default();
                    let fix = all.iter().find(|a| a["title"] == title).cloned();
                    match (fix, verb) {
                        (None, _) => {
                            let offered: Vec<&str> =
                                all.iter().filter_map(|a| a["title"].as_str()).collect();
                            note = format!(
                                "// no fix titled {title:?}; offered: {}\n",
                                serde_json::to_string(&offered).unwrap()
                            );
                            None
                        }
                        (Some(f), "fix") => Some(f),
                        (Some(f), _) => {
                            let id = f["data"]["fixId"].clone();
                            let all_fix = all
                                .iter()
                                .find(|a| a["data"]["fixAll"] == true && a["data"]["fixId"] == id)
                                .cloned();
                            match all_fix {
                                Some(a) => {
                                    note = format!(
                                        "// fix all: {}\n",
                                        a["title"].as_str().unwrap_or("")
                                    );
                                    Some(a)
                                }
                                None => {
                                    note = format!("// fix {title:?} has no fix-all\n");
                                    None
                                }
                            }
                        }
                    }
                }
            }
        }
        "refactor" => {
            let r = actions::code_actions(&cx, &mut ck, &diags, &params(vec!["refactor"], vec![]));
            let all = r.as_array().cloned().unwrap_or_default();
            let found = all.iter().find(|a| a["data"]["refactor"] == arg).cloned();
            if let Some(a) = &found {
                note = format!("// title: {}\n", a["title"].as_str().unwrap_or(""));
            }
            if found.is_none() {
                let offered: Vec<&str> = all
                    .iter()
                    .filter_map(|a| a["data"]["refactor"].as_str())
                    .collect();
                note = format!(
                    "// not offered; offered: {}\n",
                    serde_json::to_string(&offered).unwrap()
                );
            }
            found
        }
        "moveSuggestions" => {
            let r = actions::move_suggestions(&cx, &params(vec![], vec![]));
            let rel = |v: &Value| {
                let p = v.as_str().unwrap_or("");
                p.strip_prefix(&format!("{ROOT}/")).unwrap_or(p).to_string()
            };
            let files: Vec<String> = r["files"]
                .as_array()
                .map(|a| a.iter().map(rel).collect())
                .unwrap_or_default();
            return format!(
                "// newFileName: {}\n// files: {}\n",
                rel(&r["newFileName"]),
                files.join(" ")
            );
        }
        _ => {
            note = format!("// unknown action {action:?}\n");
            None
        }
    };
    match chosen {
        Some(a) => {
            apply_workspace_edit(&mut texts, &a["edit"]);
            format!("{note}{}", render(&before, &texts))
        }
        None => note,
    }
}

/// `texts` with the file `from`, or every file under the directory `from`, moved to `to`.
fn move_paths(texts: &BTreeMap<String, String>, from: &str, to: &str) -> BTreeMap<String, String> {
    texts
        .iter()
        .map(|(k, v)| {
            let k = if k == from {
                to.to_string()
            } else if let Some(rest) = k.strip_prefix(&format!("{from}/")) {
                format!("{to}/{rest}")
            } else {
                k.clone()
            };
            (k, v.clone())
        })
        .collect()
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/actions")
}

#[test]
fn action_fixtures_match_tsserver() {
    if ts_r_lsp::tsr::libs().is_none() {
        eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
        return;
    }
    let only = std::env::var("TS_R_ACTIONS_ONLY").ok();
    let update = std::env::var("TS_R_UPDATE_SNAPSHOTS").is_ok_and(|v| v == "1");
    let mut names: Vec<String> = fs::read_dir(fixture_dir())
        .unwrap()
        .filter_map(|e| {
            let n = e.ok()?.file_name().to_string_lossy().to_string();
            n.strip_suffix(".txt").map(str::to_string)
        })
        .filter(|n| only.as_deref().is_none_or(|o| n.starts_with(o)))
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let (mut matched, mut differs, mut compared) = (0, 0, 0);
    for n in &names {
        let src = fs::read_to_string(fixture_dir().join(format!("{n}.txt"))).unwrap();
        let fx = parse_fixture(&src);
        let full = run(&fx);
        if std::env::var("TS_R_ACTIONS_PRINT").is_ok() {
            eprintln!("=== {n}\n{full}");
        }
        let ours = not_offered(&full);
        let oracle = fs::read_to_string(fixture_dir().join(format!("{n}.tsserver")))
            .ok()
            .map(|t| {
                not_offered(
                    &t.split_once('\n')
                        .map_or(String::new(), |x| x.1.to_string()),
                )
            });
        // Results recorded before titles were (`// title:` lines) compare without ours.
        let ours = match &oracle {
            Some(o) if !o.starts_with("// title: ") && ours.starts_with("// title: ") => ours
                .split_once('\n')
                .map_or(String::new(), |x| x.1.to_string()),
            _ => ours,
        };
        if let Some(o) = &oracle {
            compared += 1;
            if *o == ours {
                matched += 1;
            }
        }
        if fx.dir.contains_key("differs") {
            differs += 1;
            let p = fixture_dir().join(format!("{n}.expected"));
            if update {
                fs::write(&p, &ours).unwrap();
            }
            let want = fs::read_to_string(&p).unwrap_or_default();
            if want != ours {
                failures.push(format!(
                    "{n} (vs .expected):\n--- want\n{want}--- ours\n{ours}"
                ));
            }
        } else {
            match oracle {
                Some(o) if o == ours => {}
                Some(o) => failures.push(format!("{n}:\n--- tsserver\n{o}--- ours\n{ours}")),
                None => failures.push(format!(
                    "{n}: no .tsserver (run tests/oracle/actions.js)\n--- ours\n{ours}"
                )),
            }
        }
    }
    eprintln!(
        "action fixtures: {} total, {matched}/{compared} equal to tsserver, {differs} marked @differs",
        names.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A refactor that is not offered is compared without the list of what is offered instead
/// (other refactors, which tsserver may have more of or list in another order).
fn not_offered(result: &str) -> String {
    match result.strip_prefix("// not offered; offered: ") {
        Some(rest) => format!(
            "// not offered\n{}",
            rest.split_once('\n').map_or("", |x| x.1)
        ),
        None => result.to_string(),
    }
}

/// Refactors at every cursor position, every node and every line of every fixture, and the
/// quick fixes of every diagnostic: no panics, edits never overlap, results parse (and with
/// `TS_R_ACTIONS_DEEP=1`, type-check) with no more errors than before.
/// An interactive client asked without a target gets a Move to file entry with no edit; it
/// asks again with `context.tsr.targetFile` once the user picked one.
#[test]
fn move_to_file_is_interactive() {
    if ts_r_lsp::tsr::libs().is_none() {
        return;
    }
    let fx = parse_fixture(
        "// @file: src/main.ts\n/*[*/export const a = 1;/*]*/\nexport const b = a;\n// @file: src/x.ts\nexport const x = 1;\n",
    );
    let texts: BTreeMap<String, String> = fx.files.iter().cloned().collect();
    let built = build(&fx, &texts);
    let program = &built.program;
    let checker = Checker::new(program);
    let file = FileId(program.file_index(&abs("src/main.ts")).unwrap() as u32);
    let mut ck = Ck::new(&checker, file);
    let diags = ck.check_source_file(file);
    let text = checker.file(file).text.as_str();
    let lines = LspLines::new(text);
    let (_, s, e) = fx.sel.clone().unwrap();
    let cx = Ctx {
        program,
        checker: &checker,
        file,
    };
    let ask = |ck: &mut Ck, tsr: Value| {
        let p = json!({
            "textDocument": {"uri": ts_r_lsp::uri::path_to_uri(&abs("src/main.ts"))},
            "range": lines.range(text, s as u32, e as u32),
            "context": {"diagnostics": [], "only": ["refactor.move"], "tsr": tsr},
        });
        let r = actions::code_actions(&cx, ck, &diags, &p);
        r.as_array()
            .and_then(|a| a.iter().find(|a| a["title"] == "Move to file").cloned())
    };
    assert!(
        ask(&mut ck, json!({})).is_none(),
        "not offered to a plain client"
    );
    let a = ask(&mut ck, json!({"interactive": true})).expect("offered");
    assert_eq!(a["kind"], "refactor.move.file");
    assert_eq!(a["data"]["interactive"], true);
    assert!(a.get("edit").is_none());
    let a = ask(&mut ck, json!({"targetFile": abs("src/x.ts")})).expect("edits");
    let changes = &a["edit"]["changes"];
    assert!(
        changes
            .get(ts_r_lsp::uri::path_to_uri(&abs("src/x.ts")))
            .is_some()
    );
}

#[test]
fn refactors_anywhere_are_well_formed() {
    if ts_r_lsp::tsr::libs().is_none() {
        return;
    }
    let mut names: Vec<String> = fs::read_dir(fixture_dir())
        .unwrap()
        .filter_map(|e| {
            let n = e.ok()?.file_name().to_string_lossy().to_string();
            n.strip_suffix(".txt").map(str::to_string)
        })
        .collect();
    names.sort();
    let mut offered = 0usize;
    // With TS_R_ACTIONS_DEEP=1, every distinct result is type-checked too.
    let deep = std::env::var("TS_R_ACTIONS_DEEP").is_ok_and(|v| v == "1");
    let mut results: BTreeMap<BTreeMap<String, String>, (String, String, u32, u32)> =
        BTreeMap::new();
    let mut before_errors: BTreeMap<String, usize> = BTreeMap::new();
    for n in &names {
        let fx =
            parse_fixture(&fs::read_to_string(fixture_dir().join(format!("{n}.txt"))).unwrap());
        let texts: BTreeMap<String, String> = fx.files.iter().cloned().collect();
        let built = build(&fx, &texts);
        let program = &built.program;
        let checker = Checker::new(program);
        if deep {
            before_errors.insert(n.clone(), semantic_errors(&fx, &texts));
        }
        for (name, _) in &fx.files {
            let Some(fi) = program.file_index(&abs(name)) else {
                continue;
            };
            let file = FileId(fi as u32);
            let mut ck = Ck::new(&checker, file);
            let diags = ck.check_source_file(file);
            let f = checker.file(file);
            let text = f.text.as_str();
            let lines = LspLines::new(text);
            let cx = Ctx {
                program,
                checker: &checker,
                file,
            };
            let mut ranges: Vec<(u32, u32)> = (0..=text.len() as u32)
                .filter(|&i| text.is_char_boundary(i as usize))
                .map(|i| (i, i))
                .collect();
            for i in 1..=f.ast.node_count() as u32 {
                if let Some(id) = ts_r_ast::NodeId::from_raw(i) {
                    let sp = f.ast.span(id);
                    ranges.push((sp.start, sp.end));
                }
            }
            let mut ls = 0u32;
            for line in text.split_inclusive('\n') {
                ranges.push((ls, ls + line.len() as u32));
                ls += line.len() as u32;
            }
            let mut requests: Vec<(u32, u32, Value)> = ranges
                .into_iter()
                .map(|(s, e)| (s, e, json!({"diagnostics": [], "only": ["refactor"]})))
                .collect();
            // Quick fixes for each diagnostic of the file.
            let no_rel = |_: &ts_r_diagnostics::RelatedInfo| None;
            for d in diags.iter().filter(|d| d.file == file) {
                let ld = diagnostic_to_lsp(d, text, &lines, DiagKind::Semantic, &no_rel);
                requests.push((
                    d.span.start,
                    d.span.end,
                    json!({"diagnostics": [ld], "only": ["quickfix"]}),
                ));
            }
            for (s, e, context) in requests {
                let params = json!({
                    "textDocument": {"uri": ts_r_lsp::uri::path_to_uri(&abs(name))},
                    "range": lines.range(text, s, e),
                    "context": context,
                });
                let r = actions::code_actions(&cx, &mut ck, &diags, &params);
                for a in r.as_array().unwrap() {
                    offered += 1;
                    // The result parses with no more syntax errors than before.
                    let mut after = texts.clone();
                    apply_workspace_edit(&mut after, &a["edit"]);
                    if deep {
                        let title = a["title"].as_str().unwrap_or("").to_string();
                        results
                            .entry(after.clone())
                            .or_insert((n.clone(), title, s, e));
                    }
                    for (fname, t) in &after {
                        let old = texts.get(fname).map_or("", String::as_str);
                        if old == t {
                            continue;
                        }
                        let errs =
                            |src: &str| ts_r_parser::parse_path(fname, src).diagnostics.len();
                        assert!(
                            errs(t) <= errs(old),
                            "{n}: {} at {s}..{e} breaks {fname}:\n{t}",
                            a["title"]
                        );
                    }
                    let changes = a["edit"]["changes"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    let mut lists: Vec<Value> = changes.into_iter().map(|(_, v)| v).collect();
                    for dc in a["edit"]["documentChanges"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if dc.get("edits").is_some() {
                            lists.push(dc["edits"].clone());
                        }
                    }
                    for list in lists {
                        let mut rs: Vec<((u32, u32), (u32, u32))> = list
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|x| {
                                let p = |v: &Value| {
                                    let q = Pos::from_lsp(v).unwrap();
                                    (q.line, q.character)
                                };
                                (p(&x["range"]["start"]), p(&x["range"]["end"]))
                            })
                            .collect();
                        rs.sort();
                        for w in rs.windows(2) {
                            assert!(
                                w[0].1 <= w[1].0,
                                "{n}: {} at {s}..{e} has overlapping edits: {a}",
                                a["title"]
                            );
                        }
                    }
                }
            }
        }
    }
    eprintln!("refactor sweep: {offered} actions offered");
    if deep {
        let mut bad = Vec::new();
        for (after, (n, title, s, e)) in &results {
            let fx =
                parse_fixture(&fs::read_to_string(fixture_dir().join(format!("{n}.txt"))).unwrap());
            let got = semantic_errors(&fx, after);
            if got > before_errors[n] {
                let shown: Vec<String> = after
                    .iter()
                    .map(|(k, v)| format!("// @file: {k}\n{v}"))
                    .collect();
                bad.push(format!(
                    "{n}: {title} at {s}..{e}: {got} errors (was {})\n{}",
                    before_errors[n],
                    shown.join("")
                ));
            }
        }
        eprintln!("refactor sweep: {} distinct results checked", results.len());
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Semantic and syntax errors of the program of `texts`.
fn semantic_errors(fx: &Fixture, texts: &BTreeMap<String, String>) -> usize {
    let built = build(fx, texts);
    let program = &built.program;
    let checker = Checker::new(program);
    let mut count = 0;
    for name in texts.keys() {
        let Some(fi) = program.file_index(&abs(name)) else {
            continue;
        };
        let file = FileId(fi as u32);
        let mut ck = Ck::new(&checker, file);
        // JavaScript parameters are untyped (implicit `any` under `checkJs` + `strict`), as
        // in tsserver's results: extracting a function there adds such errors by design.
        let js = checker.file(file).is_js();
        count += ck
            .check_source_file(file)
            .iter()
            .filter(|d| !(js && matches!(d.code, 7006 | 7031)))
            .count()
            + program.files[fi].syntax.len();
    }
    count
}
