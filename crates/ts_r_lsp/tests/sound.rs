//! Sound mode in the editor (record 0007, "Reporting"): TSR diagnostics for files where sound
//! mode applies (`ts-r.toml`, `@ts-r` pragma), their source, code and rule link, the quick
//! fixes, and `ts-r/soundLevel`. Through a real LSP front on the ts-r backend. Skipped when
//! the lib files are missing (`cargo xtask upstream sync --minimal`).
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use ts_r_lsp::client::{Dialect, ScriptedClient, did_open_params, initialize_params};

ts_r_testing::cap_memory!();

const BIN: &str = env!("CARGO_BIN_EXE_ts-r-lsp");
const T: Duration = Duration::from_secs(30);

struct Env {
    dir: PathBuf,
    proj: PathBuf,
}

impl Env {
    fn new(name: &str, files: &[(&str, &str)]) -> Env {
        let dir = PathBuf::from(format!("/tmp/tsr-sound-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        fs::create_dir_all(&proj).unwrap();
        for (p, text) in files {
            let path = proj.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let proj = fs::canonicalize(&proj).unwrap();
        Env { dir, proj }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.proj)
            .env("TS_R_CACHE_DIR", self.dir.join("cache"))
            .env("TS_R_LSP_LINGER_MS", "300")
            .env("TS_R_LSP_TYPE_BACKEND", "ts-r")
            .env("TS_R_LSP_PARITY", "off")
            .env_remove("TS_R_LSP_TSSERVER_CMD")
            .env_remove("TS_R_LSP_NATIVE_CMD")
            .env_remove("TS_R_WORKSPACE_ROOT");
        c
    }

    fn uri(&self, name: &str) -> String {
        ts_r_lsp::uri::path_to_uri(&self.proj.join(name).to_string_lossy())
    }

    fn text(&self, name: &str) -> String {
        fs::read_to_string(self.proj.join(name)).unwrap()
    }

    fn lsp(&self) -> ScriptedClient {
        let mut c =
            ScriptedClient::spawn(self.cmd(&["--name", "w", "--lint", "off"]), Dialect::Lsp)
                .unwrap();
        let r = c
            .call("initialize", initialize_params(&self.proj), T)
            .expect("initialize answered");
        assert!(r.get("result").is_some(), "initialize failed: {r}");
        c.notify("initialized", json!({})).unwrap();
        c
    }

    fn open(&self, c: &mut ScriptedClient, name: &str) {
        let path = self.proj.join(name);
        let text = fs::read_to_string(&path).unwrap();
        c.notify("textDocument/didOpen", did_open_params(&path, &text))
            .unwrap();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self
            .cmd(&["stop", "--root", &self.proj.to_string_lossy()])
            .output();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn libs_present() -> bool {
    let ok = ts_r_lsp::tsr::libs().is_some();
    if !ok {
        eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
    }
    ok
}

fn diags_for(
    c: &mut ScriptedClient,
    uri: &str,
    pred: impl Fn(&[Value]) -> bool,
) -> Option<Vec<Value>> {
    let deadline = Instant::now() + T;
    while Instant::now() < deadline {
        let m = c.wait_for(deadline - Instant::now(), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri
        })?;
        let d = m["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if pred(&d) {
            return Some(d);
        }
    }
    None
}

fn sound(d: &[Value]) -> Vec<&Value> {
    d.iter().filter(|x| x["source"] == "ts-r sound").collect()
}

fn code_list(d: &[Value]) -> Vec<String> {
    sound(d)
        .iter()
        .filter_map(|x| x["code"].as_str().map(str::to_owned))
        .collect()
}

/// Applies LSP text edits (ASCII text) to `text`.
fn apply(text: &str, edits: &[Value]) -> String {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let off = |p: &Value| {
        let l = p["line"].as_u64().unwrap() as usize;
        starts.get(l).copied().unwrap_or(text.len()) + p["character"].as_u64().unwrap() as usize
    };
    let mut es: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                off(&e["range"]["start"]),
                off(&e["range"]["end"]),
                e["newText"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    es.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = text.to_owned();
    for (s, e, t) in es {
        out.replace_range(s..e, &t);
    }
    out
}

/// The quick fixes offered for `diags` of `name`.
fn actions(c: &mut ScriptedClient, env: &Env, name: &str, diags: Vec<&Value>) -> Vec<Value> {
    let range = diags[0]["range"].clone();
    let r = c
        .call(
            "textDocument/codeAction",
            json!({"textDocument": {"uri": env.uri(name)}, "range": range,
                   "context": {"diagnostics": diags}}),
            T,
        )
        .expect("codeAction answered");
    assert!(r.get("error").is_none(), "{r}");
    r["result"].as_array().cloned().unwrap_or_default()
}

fn fixed(env: &Env, name: &str, actions: &[Value], title_part: &str) -> String {
    let a = actions
        .iter()
        .find(|a| a["title"].as_str().unwrap_or("").contains(title_part))
        .unwrap_or_else(|| panic!("no '{title_part}' fix in {actions:?}"));
    let edits = a["edit"]["changes"][env.uri(name)].as_array().unwrap();
    apply(&env.text(name), edits)
}

const TSCONFIG: &str = r#"{"compilerOptions": {"strict": true, "module": "nodenext", "target": "es2022", "noEmit": true}, "include": ["src", "other"]}"#;

const SOUND_TOML: &str = "[sound]\nlevel = \"sound\"\ninclude = [\"src/**\"]\n";

const A: &str = "\
export interface User { id: number; name: string }
export function load(text: string): User {
  const raw: unknown = JSON.parse(text);
  return raw as User;
}
export function first(xs: string[]): string {
  return xs[0]!;
}
async function save(): Promise<void> {}
export async function go(): Promise<void> {
  save();
}
";

#[test]
fn sound_files_get_tsr_diagnostics_and_quick_fixes() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "diags",
        &[
            ("tsconfig.json", TSCONFIG),
            ("ts-r.toml", SOUND_TOML),
            ("src/a.ts", A),
            (
                "src/off.ts",
                "// @ts-r off\nexport function f(x: unknown): string {\n  return x as string;\n}\n",
            ),
            (
                "other/b.ts",
                "export function g(x: unknown): string {\n  return x as string;\n}\n",
            ),
            (
                "other/c.ts",
                "// @ts-r sound\nexport function h(x: unknown): string {\n  return x as string;\n}\n",
            ),
        ],
    );
    let mut c = env.lsp();
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    let d = diags_for(&mut c, &ua, |d| {
        code_list(d).contains(&"TSR0201".to_owned())
    })
    .expect("a.ts sound diagnostics");
    let codes = code_list(&d);
    for want in ["TSR0201", "TSR0301", "TSR1001"] {
        assert!(codes.contains(&want.to_owned()), "{want} missing: {d:?}");
    }
    let s0201 = sound(&d)
        .into_iter()
        .find(|x| x["code"] == "TSR0201")
        .unwrap();
    assert_eq!(s0201["range"]["start"], json!({"line": 3, "character": 9}));
    assert_eq!(
        s0201["codeDescription"]["href"],
        "https://webb-ventures.github.io/tsc-rust/sound/rules/S-02/"
    );
    assert_eq!(s0201["severity"], 1);
    assert!(
        s0201["message"]
            .as_str()
            .unwrap()
            .starts_with("Type assertion from 'unknown' to 'User' narrows the type.")
    );

    // ts-r/soundLevel: the file's level and where it came from.
    let r = c
        .call("ts-r/soundLevel", json!({"textDocument": {"uri": ua}}), T)
        .unwrap();
    assert_eq!(r["result"]["level"], "sound", "{r}");
    assert!(
        r["result"]["config"]
            .as_str()
            .unwrap_or("")
            .ends_with("/ts-r.toml"),
        "{r}"
    );

    // Quick fixes: parse<T> (with the import), expect(...), await, and the hatch.
    let acts = actions(&mut c, &env, "src/a.ts", vec![s0201]);
    let out = fixed(&env, "src/a.ts", &acts, "parse<User>");
    assert!(
        out.starts_with("import { parse } from \"@ts-r.dev/guard\";\nexport interface User"),
        "{out}"
    );
    assert!(out.contains("  return parse<User>(raw);\n"), "{out}");
    let out = fixed(&env, "src/a.ts", &acts, "@ts-r-unsafe S-02");
    assert!(
        out.contains("  const raw: unknown = JSON.parse(text);\n  // @ts-r-unsafe S-02: \n  return raw as User;"),
        "{out}"
    );
    let s0301 = sound(&d)
        .into_iter()
        .find(|x| x["code"] == "TSR0301")
        .unwrap();
    let acts = actions(&mut c, &env, "src/a.ts", vec![s0301]);
    let out = fixed(&env, "src/a.ts", &acts, "expect");
    assert!(
        out.contains("  return expect(xs[0], \"xs[0] is set\");\n"),
        "{out}"
    );
    assert!(
        out.starts_with("import { expect } from \"@ts-r.dev/guard\";\n"),
        "{out}"
    );
    let s1001 = sound(&d)
        .into_iter()
        .find(|x| x["code"] == "TSR1001")
        .unwrap();
    let acts = actions(&mut c, &env, "src/a.ts", vec![s1001]);
    let out = fixed(&env, "src/a.ts", &acts, "await");
    assert!(out.contains("  await save();\n"), "{out}");
    let out = fixed(&env, "src/a.ts", &acts, "void");
    assert!(
        out.contains("  void save().catch(console.error);\n"),
        "{out}"
    );

    // `// @ts-r off` and files the configuration does not include: type diagnostics only.
    for name in ["src/off.ts", "other/b.ts"] {
        let u = env.uri(name);
        env.open(&mut c, name);
        let d = diags_for(&mut c, &u, |_| true).expect("published");
        assert!(sound(&d).is_empty(), "{name}: {d:?}");
        let r = c
            .call("ts-r/soundLevel", json!({"textDocument": {"uri": u}}), T)
            .unwrap();
        assert_eq!(r["result"]["level"], "off", "{name}: {r}");
    }
    // A `// @ts-r sound` pragma opts a file in without the configuration.
    let uc = env.uri("other/c.ts");
    env.open(&mut c, "other/c.ts");
    let d = diags_for(&mut c, &uc, |d| !sound(d).is_empty()).expect("c.ts sound diagnostics");
    assert_eq!(code_list(&d), vec!["TSR0201".to_owned()], "{d:?}");
    c.shutdown();
}

#[test]
fn hatches_throws_and_numeric_codes() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "hatch",
        &[
            ("tsconfig.json", TSCONFIG),
            (
                "ts-r.toml",
                // TSR1302 (an exported function without `@throws`) is opt-in.
                "[sound]\nlevel = \"sound\"\ninclude = [\"src/**\"]\ncode-prefix = \"TS\"\nexports-declare-throws = true\n",
            ),
            (
                "src/a.ts",
                "// @ts-r-unsafe S-03: nothing here needs it\nexport const n: number = 1;\n\
export function boom(x: number): number {\n  if (x < 0) throw new RangeError(\"negative\");\n  return x;\n}\n",
            ),
        ],
    );
    let mut c = env.lsp();
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    let d = diags_for(&mut c, &ua, |d| sound(d).len() >= 2).expect("sound diagnostics");
    // code-prefix = "TS": numeric codes (100000 + n).
    let nums: Vec<u64> = sound(&d)
        .iter()
        .filter_map(|x| x["code"].as_u64())
        .collect();
    assert!(nums.contains(&100_002) && nums.contains(&101_302), "{d:?}");
    let unused = sound(&d)
        .into_iter()
        .find(|x| x["code"] == 100_002)
        .unwrap();
    let acts = actions(&mut c, &env, "src/a.ts", vec![unused]);
    let out = fixed(&env, "src/a.ts", &acts, "Remove the unused");
    assert!(out.starts_with("export const n: number = 1;\n"), "{out}");
    let throws = sound(&d)
        .into_iter()
        .find(|x| x["code"] == 101_302)
        .unwrap();
    let acts = actions(&mut c, &env, "src/a.ts", vec![throws]);
    let out = fixed(&env, "src/a.ts", &acts, "@throws");
    assert!(
        out.contains("/**\n * @throws {RangeError}\n */\nexport function boom"),
        "{out}"
    );
    c.shutdown();
}

#[test]
fn pragma_problems_in_off_files_are_reported() {
    if !libs_present() {
        return;
    }
    // No sound configuration: both files are off, but a misspelt pragma (an attempted opt-in)
    // and a late one are still reported.
    let env = Env::new(
        "pragma-off",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", "// @ts-r soud\nexport const n: number = 1;\n"),
            (
                "src/b.ts",
                "export const m: number = 1;\n// @ts-r sound\nexport const k: number = m;\n",
            ),
        ],
    );
    let mut c = env.lsp();
    let (ua, ub) = (env.uri("src/a.ts"), env.uri("src/b.ts"));
    env.open(&mut c, "src/a.ts");
    let d = diags_for(&mut c, &ua, |d| !sound(d).is_empty()).expect("TSR0004");
    assert_eq!(code_list(&d), ["TSR0004"], "{d:?}");
    env.open(&mut c, "src/b.ts");
    let d = diags_for(&mut c, &ub, |d| !sound(d).is_empty()).expect("TSR0005");
    assert_eq!(code_list(&d), ["TSR0005"], "{d:?}");
    c.shutdown();
}
