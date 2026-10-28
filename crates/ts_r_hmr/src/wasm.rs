//! WebAssembly imports, served the way Vite serves them in dev
//! (`vite: packages/vite/src/node/plugins/wasm.ts`):
//!
//! - `import init from './a.wasm?init'`: a default export `opts => instance`, which
//!   instantiates the file's URL with `opts` as the import object;
//! - `import { f } from './a.wasm'` (ES module integration): a glue module that imports
//!   whatever the wasm imports, instantiates it once (top-level `await`) and exports its
//!   exports by name. A module with exported globals gets a thin wrapper in front that
//!   re-exports the glue (`?vite-wasm-instance`) and replaces each global with its value;
//!   a wasm that imports a global imports the `WebAssembly.Global` itself from the other
//!   module's instance.
//!
//! The import and export lists come from a small reader of the binary's import (2) and
//! export (7) sections: every read is bounds-checked and malformed input is an error,
//! never a panic. Import modules the engine provides itself (`wasm:js-string`,
//! `wasm:js/string-constants`) are not imported.

use std::path::{Path, PathBuf};

/// The helper every glue module imports (Vite's URL for it, so pages match).
pub const HELPER_URL: &str = "/@id/__x00__vite/wasm-helper.js";

/// What the browser is asked to compile with: JS string builtins and imported string
/// constants, which also makes those import modules engine-provided.
const COMPILE_OPTIONS: &str =
    r#"{"builtins":["js-string"],"importedStringConstants":"wasm:js/string-constants"}"#;

/// Import modules the engine supplies under [`COMPILE_OPTIONS`].
const RESERVED_MODULES: &[&str] = &["wasm:js-string", "wasm:js/string-constants"];

/// The helper module: `default(opts, url)` fetches (or decodes a `data:` URL), instantiates
/// and resolves to the `WebAssembly.Instance`.
pub fn helper() -> String {
    format!(
        r#"const compileOptions = {COMPILE_OPTIONS};
async function instantiateUrl(url, imports) {{
  const res = await fetch(url);
  const type = res.headers.get("Content-Type") || "";
  if ("instantiateStreaming" in WebAssembly && type.startsWith("application/wasm")) {{
    return WebAssembly.instantiateStreaming(res, imports, compileOptions);
  }}
  return WebAssembly.instantiate(await res.arrayBuffer(), imports, compileOptions);
}}
export default async function initWasm(imports = {{}}, url) {{
  let result;
  if (url.startsWith("data:")) {{
    const text = atob(url.replace(/^data:.*?base64,/, ""));
    const bytes = new Uint8Array(text.length);
    for (let i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i);
    result = await WebAssembly.instantiate(bytes, imports, compileOptions);
  }} else {{
    result = await instantiateUrl(url, imports);
  }}
  return result.instance;
}}
"#
    )
}

/// One imported or exported item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub global: bool,
}

/// The import modules (in first-use order, each with its items) and the exports of a module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WasmInfo {
    pub imports: Vec<(String, Vec<Item>)>,
    pub exports: Vec<Item>,
}

impl WasmInfo {
    pub fn has_global_export(&self) -> bool {
        self.exports.iter().any(|e| e.global)
    }
}

const KIND_GLOBAL: u8 = 3;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self
            .bytes
            .get(self.at)
            .ok_or("unexpected end of the module")?;
        self.at += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.bytes.len())
            .ok_or("unexpected end of the module")?;
        let s = &self.bytes[self.at..end];
        self.at = end;
        Ok(s)
    }

    /// An unsigned LEB128 of at most `max_bytes` bytes.
    fn leb(&mut self, max_bytes: u32) -> Result<u64, String> {
        let mut value = 0u64;
        for i in 0..max_bytes {
            let b = self.byte()?;
            if i < 10 {
                value |= u64::from(b & 0x7f) << (7 * i).min(63);
            }
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("integer representation too long".to_owned())
    }

    fn u32(&mut self) -> Result<u32, String> {
        u32::try_from(self.leb(5)?).map_err(|_| "integer too large".to_owned())
    }

    fn name(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).map_err(|_| "malformed UTF-8 name".to_owned())
    }

    /// A heap type: an abstract type (one negative byte) or a type index (s33).
    fn heap_type(&mut self) -> Result<(), String> {
        self.leb(5).map(|_| ())
    }

    fn ref_type(&mut self, first: u8) -> Result<(), String> {
        match first {
            0x63 | 0x64 => self.heap_type(),
            0x40..=0x7f => Ok(()),
            b => Err(format!("invalid value type 0x{b:02x}")),
        }
    }

    fn val_type(&mut self) -> Result<(), String> {
        let b = self.byte()?;
        self.ref_type(b)
    }

    /// Limits: flags (bit 0: a maximum; bit 2: 64-bit; bit 3: a custom page size).
    fn limits(&mut self) -> Result<(), String> {
        let flags = self.byte()?;
        let width = if flags & 0x04 != 0 { 10 } else { 5 };
        self.leb(width)?;
        if flags & 0x01 != 0 {
            self.leb(width)?;
        }
        if flags & 0x08 != 0 {
            self.u32()?;
        }
        Ok(())
    }
}

/// Reads the import and export sections of a wasm binary.
pub fn parse(bytes: &[u8]) -> Result<WasmInfo, String> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
        return Err("not a WebAssembly module (bad magic number)".to_owned());
    }
    if bytes[4..8] != [1, 0, 0, 0] {
        return Err("unsupported WebAssembly binary version".to_owned());
    }
    let mut r = Reader { bytes, at: 8 };
    let mut info = WasmInfo::default();
    while r.at < bytes.len() {
        let id = r.byte()?;
        let size = r.u32()? as usize;
        let body = r.take(size)?;
        let mut s = Reader { bytes: body, at: 0 };
        match id {
            2 => read_imports(&mut s, &mut info)?,
            7 => {
                let n = s.u32()?;
                for _ in 0..n {
                    let name = s.name()?;
                    let kind = s.byte()?;
                    s.u32()?;
                    info.exports.push(Item {
                        name,
                        global: kind == KIND_GLOBAL,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(info)
}

fn read_imports(s: &mut Reader<'_>, info: &mut WasmInfo) -> Result<(), String> {
    let n = s.u32()?;
    for _ in 0..n {
        let module = s.name()?;
        let name = s.name()?;
        let kind = s.byte()?;
        match kind {
            0 => {
                s.u32()?;
            }
            1 => {
                let t = s.byte()?;
                s.ref_type(t)?;
                s.limits()?;
            }
            2 => s.limits()?,
            3 => {
                s.val_type()?;
                s.byte()?;
            }
            4 => {
                s.byte()?;
                s.u32()?;
            }
            k => return Err(format!("invalid import kind 0x{k:02x}")),
        }
        if RESERVED_MODULES.contains(&module.as_str()) {
            continue;
        }
        let item = Item {
            name,
            global: kind == KIND_GLOBAL,
        };
        match info.imports.iter_mut().find(|(m, _)| *m == module) {
            Some((_, items)) => items.push(item),
            None => info.imports.push((module, vec![item])),
        }
    }
    Ok(())
}

/// The `?init` module: `opts => initWasm(opts, url)`.
pub fn init_module(url: &str) -> String {
    format!(
        "import initWasm from {}\nexport default opts => initWasm(opts, {})\n",
        js(HELPER_URL),
        js(url)
    )
}

/// The specifier, relative to the wasm file, of its instance module.
pub fn instance_specifier(wasm: &Path) -> String {
    let name = wasm.file_name().and_then(|n| n.to_str()).unwrap_or("");
    format!("./{name}?vite-wasm-instance")
}

/// The module that instantiates the wasm at `url` and exports its exports as they are
/// (globals as `WebAssembly.Global`s).
pub fn instance_module(info: &WasmInfo, url: &str) -> String {
    let mut out = format!(
        "import __tsr_initWasm from {}\nconst __tsr_wasmUrl = {}\n",
        js(HELPER_URL),
        js(url)
    );
    let mut object = Vec::new();
    for (i, (from, items)) in info.imports.iter().enumerate() {
        let mut fields = Vec::new();
        for (global, ns, spec) in [
            (false, format!("__tsr_wasmImport_{i}"), from.clone()),
            (
                true,
                format!("__tsr_wasmImportInstance_{i}"),
                format!("{from}?vite-wasm-instance"),
            ),
        ] {
            let names: Vec<&Item> = items.iter().filter(|it| it.global == global).collect();
            if names.is_empty() {
                continue;
            }
            out.push_str(&format!("import * as {ns} from {};\n", js(&spec)));
            for it in names {
                fields.push(format!("{}: {ns}[{}]", js(&it.name), js(&it.name)));
            }
        }
        object.push(format!("{}: {}", js(from), simple_object(&fields)));
    }
    out.push_str(&format!(
        "const __tsr_wasmModule = (await __tsr_initWasm({}, __tsr_wasmUrl)).exports;\n",
        simple_object(&object)
    ));
    if info.exports.is_empty() {
        return out;
    }
    let mut locals = Vec::new();
    let mut exported = Vec::new();
    for (i, e) in info.exports.iter().enumerate() {
        if own_binding(&e.name) {
            locals.push(e.name.clone());
            exported.push(e.name.clone());
        } else {
            let local = format!("__tsr_wasmExport_{i}");
            locals.push(format!("{}: {local}", js(&e.name)));
            exported.push(format!("{local} as {}", js(&e.name)));
        }
    }
    if locals == exported {
        out.push_str(&format!(
            "export const {{ {} }} = __tsr_wasmModule;\n",
            locals.join(", ")
        ));
    } else {
        out.push_str(&format!(
            "const {{ {} }} = __tsr_wasmModule;\nexport {{ {} }};\n",
            locals.join(", "),
            exported.join(", ")
        ));
    }
    out
}

/// The module a direct import of a wasm with exported globals gets: everything from the
/// instance module, with each global replaced by its current value.
pub fn wrapper_module(info: &WasmInfo, instance: &str) -> String {
    let inst = js(instance);
    let mut out = format!("export * from {inst};\n");
    if info.exports.iter().any(|e| e.name == "default") {
        out.push_str(&format!("export {{ default }} from {inst};\n"));
    }
    let mut imports = Vec::new();
    let mut bindings = Vec::new();
    let mut unwraps = String::new();
    let mut exports = Vec::new();
    for (i, e) in info.exports.iter().enumerate() {
        if !e.global || e.name == "default" {
            continue;
        }
        let alias = format!("__tsr_wasmGlobal_{i}");
        let (imported, binding) = if own_binding(&e.name) {
            (e.name.clone(), e.name.clone())
        } else {
            (js(&e.name), format!("__tsr_wasmGlobalValue_{i}"))
        };
        imports.push(format!("{imported} as {alias}"));
        unwraps.push_str(&format!(
            "try {{ {binding} = {alias}.value; }} catch {{}}\n"
        ));
        exports.push(if binding == e.name {
            binding.clone()
        } else {
            format!("{binding} as {}", js(&e.name))
        });
        bindings.push(binding);
    }
    if !bindings.is_empty() {
        out.push_str(&format!(
            "import {{ {} }} from {inst};\nlet {};\n{unwraps}export {{ {} }};\n",
            imports.join(", "),
            bindings.join(", "),
            exports.join(", ")
        ));
    }
    out
}

fn simple_object(fields: &[String]) -> String {
    match fields.is_empty() {
        true => "{}".to_owned(),
        false => format!("{{ {} }}", fields.join(", ")),
    }
}

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Whether an export can be bound under its own name in the glue modules: it can be a
/// `const` binding and can't collide with the names they generate (all `__tsr_`), which
/// such exports get instead, aliased back to the export's name.
fn own_binding(name: &str) -> bool {
    is_declarable(name) && !name.starts_with("__tsr_")
}

/// Whether `name` can be a `const` binding: an ASCII identifier that isn't reserved.
fn is_declarable(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "arguments",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "eval",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ];
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !RESERVED.contains(&name)
}

/// The graph files of a wasm's JS modules: the direct-import module and the instance
/// module, beside the wasm so its imports resolve from its directory.
const GLUE_MARK: &str = "?wasm-module.js";
const INSTANCE_MARK: &str = "?wasm-instance.js";

pub fn glue_file(wasm: &Path, instance: bool) -> PathBuf {
    let mark = if instance { INSTANCE_MARK } else { GLUE_MARK };
    PathBuf::from(format!("{}{mark}", wasm.display()))
}

/// The wasm behind a [`glue_file`] path, and whether it is the instance module.
pub fn split_glue_file(file: &Path) -> Option<(PathBuf, bool)> {
    let s = file.to_str()?;
    if let Some(w) = s.strip_suffix(INSTANCE_MARK) {
        return Some((PathBuf::from(w), true));
    }
    s.strip_suffix(GLUE_MARK).map(|w| (PathBuf::from(w), false))
}

/// The source of a [`glue_file`]: the wrapper when a direct import meets exported globals,
/// else the instance module.
pub fn glue_source(wasm: &Path, instance: bool, url: &str) -> Result<String, String> {
    let bytes = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
    let info = parse(&bytes)
        .map_err(|e| format!("Failed to parse WASM file \"{}\": {e}", wasm.display()))?;
    if !instance && info.has_global_export() {
        return Ok(wrapper_module(&info, &instance_specifier(wasm)));
    }
    Ok(instance_module(&info, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leb(mut v: u32, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn name(s: &str, out: &mut Vec<u8>) {
        leb(s.len() as u32, out);
        out.extend_from_slice(s.as_bytes());
    }

    fn section(id: u8, body: &[u8], out: &mut Vec<u8>) {
        out.push(id);
        leb(body.len() as u32, out);
        out.extend_from_slice(body);
    }

    /// A module with the given imports (module, name, kind descriptor bytes) and exports.
    fn module(imports: &[(&str, &str, &[u8])], exports: &[(&str, u8)]) -> Vec<u8> {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        // A type section first, to check unknown sections are skipped.
        section(1, &[1, 0x60, 0, 0], &mut out);
        let mut body = Vec::new();
        leb(imports.len() as u32, &mut body);
        for (m, n, desc) in imports {
            name(m, &mut body);
            name(n, &mut body);
            body.extend_from_slice(desc);
        }
        section(2, &body, &mut out);
        let mut body = Vec::new();
        leb(exports.len() as u32, &mut body);
        for (n, kind) in exports {
            name(n, &mut body);
            body.push(*kind);
            body.push(0);
        }
        section(7, &body, &mut out);
        out
    }

    #[test]
    fn reads_imports_and_exports() {
        let bytes = module(
            &[
                ("./imports.js", "imported_func", &[0, 0]),
                ("./imports.js", "table", &[1, 0x70, 0x01, 1, 2]),
                ("env", "mem", &[2, 0x04, 0x80, 0x01]),
                ("./global.wasm", "answer", &[3, 0x7f, 0]),
                ("wasm:js-string", "length", &[0, 0]),
                ("wasm:js/string-constants", "hi", &[3, 0x64, 0x6f, 0]),
                ("env", "tag", &[4, 0, 0]),
            ],
            &[("add", 0), ("answer", 3), ("default", 0), ("a-b", 2)],
        );
        let info = parse(&bytes).unwrap();
        let item = |n: &str, global| Item {
            name: n.to_owned(),
            global,
        };
        assert_eq!(
            info.imports,
            vec![
                (
                    "./imports.js".to_owned(),
                    vec![item("imported_func", false), item("table", false)]
                ),
                (
                    "env".to_owned(),
                    vec![item("mem", false), item("tag", false)]
                ),
                ("./global.wasm".to_owned(), vec![item("answer", true)]),
            ]
        );
        assert_eq!(
            info.exports,
            vec![
                item("add", false),
                item("answer", true),
                item("default", false),
                item("a-b", false)
            ]
        );
    }

    #[test]
    fn malformed_modules_are_errors() {
        assert!(parse(b"").is_err());
        assert!(parse(b"\0asm\x02\0\0\0").is_err());
        let good = module(&[("m", "f", &[0, 0])], &[("f", 0)]);
        // Every truncation fails cleanly or parses a prefix; none panics.
        for n in 0..good.len() {
            let _ = parse(&good[..n]);
        }
        let mut bad = b"\0asm\x01\0\0\0".to_vec();
        section(2, &[1, 1, b'm', 1, b'f', 9], &mut bad);
        assert!(parse(&bad).unwrap_err().contains("import kind"));
        let mut long = b"\0asm\x01\0\0\0\x02".to_vec();
        long.extend_from_slice(&[0xff; 12]);
        assert!(parse(&long).is_err());
    }

    #[test]
    fn instance_module_imports_and_exports() {
        let info = WasmInfo {
            imports: vec![
                (
                    "./imports.js".to_owned(),
                    vec![Item {
                        name: "imported_func".to_owned(),
                        global: false,
                    }],
                ),
                (
                    "./global.wasm".to_owned(),
                    vec![Item {
                        name: "g".to_owned(),
                        global: true,
                    }],
                ),
            ],
            exports: vec![
                Item {
                    name: "add".to_owned(),
                    global: false,
                },
                Item {
                    name: "not-ident".to_owned(),
                    global: false,
                },
            ],
        };
        let out = instance_module(&info, "/add.wasm");
        assert!(
            out.contains("import * as __tsr_wasmImport_0 from \"./imports.js\";"),
            "{out}"
        );
        assert!(
            out.contains(
                "import * as __tsr_wasmImportInstance_1 from \"./global.wasm?vite-wasm-instance\";"
            ),
            "{out}"
        );
        assert!(out.contains(
            "{ \"./imports.js\": { \"imported_func\": __tsr_wasmImport_0[\"imported_func\"] }, \"./global.wasm\": { \"g\": __tsr_wasmImportInstance_1[\"g\"] } }"
        ), "{out}");
        assert!(
            out.contains("const { add, \"not-ident\": __tsr_wasmExport_1 } = __tsr_wasmModule;"),
            "{out}"
        );
        assert!(
            out.contains("export { add, __tsr_wasmExport_1 as \"not-ident\" };"),
            "{out}"
        );

        let plain = instance_module(
            &WasmInfo {
                imports: vec![],
                exports: vec![Item {
                    name: "add".to_owned(),
                    global: false,
                }],
            },
            "/a.wasm",
        );
        assert!(
            plain.contains("(await __tsr_initWasm({}, __tsr_wasmUrl))"),
            "{plain}"
        );
        assert!(
            plain.contains("export const { add } = __tsr_wasmModule;"),
            "{plain}"
        );
        assert!(!instance_module(&WasmInfo::default(), "/a.wasm").contains("export "));

        // Exports named like the glue's own bindings are aliased, not redeclared.
        let clash = |name: &str, global: bool| Item {
            name: name.to_owned(),
            global,
        };
        let info = WasmInfo {
            imports: vec![],
            exports: vec![
                clash("__tsr_wasmUrl", false),
                clash("__tsr_wasmModule", false),
                clash("__tsr_wasmExport_0", false),
                clash("__tsr_wasmGlobal_0", true),
            ],
        };
        let out = instance_module(&info, "/a.wasm");
        assert!(
            out.contains("const { \"__tsr_wasmUrl\": __tsr_wasmExport_0, \"__tsr_wasmModule\": __tsr_wasmExport_1, \"__tsr_wasmExport_0\": __tsr_wasmExport_2, \"__tsr_wasmGlobal_0\": __tsr_wasmExport_3 } = __tsr_wasmModule;"),
            "{out}"
        );
        assert!(
            out.contains("export { __tsr_wasmExport_0 as \"__tsr_wasmUrl\", __tsr_wasmExport_1 as \"__tsr_wasmModule\", __tsr_wasmExport_2 as \"__tsr_wasmExport_0\", __tsr_wasmExport_3 as \"__tsr_wasmGlobal_0\" };"),
            "{out}"
        );
        let wrapper = wrapper_module(&info, "./a.wasm?vite-wasm-instance");
        assert!(
            wrapper.contains("import { \"__tsr_wasmGlobal_0\" as __tsr_wasmGlobal_3 }"),
            "{wrapper}"
        );
        assert!(
            wrapper.contains("export { __tsr_wasmGlobalValue_3 as \"__tsr_wasmGlobal_0\" };"),
            "{wrapper}"
        );
    }

    #[test]
    fn wrapper_unwraps_globals() {
        let info = WasmInfo {
            imports: vec![],
            exports: vec![
                Item {
                    name: "answer".to_owned(),
                    global: true,
                },
                Item {
                    name: "f".to_owned(),
                    global: false,
                },
                Item {
                    name: "x-y".to_owned(),
                    global: true,
                },
            ],
        };
        let out = wrapper_module(&info, "./g.wasm?vite-wasm-instance");
        assert_eq!(
            out,
            "export * from \"./g.wasm?vite-wasm-instance\";\n\
             import { answer as __tsr_wasmGlobal_0, \"x-y\" as __tsr_wasmGlobal_2 } from \"./g.wasm?vite-wasm-instance\";\n\
             let answer, __tsr_wasmGlobalValue_2;\n\
             try { answer = __tsr_wasmGlobal_0.value; } catch {}\n\
             try { __tsr_wasmGlobalValue_2 = __tsr_wasmGlobal_2.value; } catch {}\n\
             export { answer, __tsr_wasmGlobalValue_2 as \"x-y\" };\n"
        );
    }

    #[test]
    fn glue_files_round_trip() {
        let w = Path::new("/app/src/add.wasm");
        for instance in [false, true] {
            let f = glue_file(w, instance);
            assert_eq!(f.parent(), Some(Path::new("/app/src")));
            assert_eq!(split_glue_file(&f), Some((w.to_path_buf(), instance)));
        }
        assert_eq!(split_glue_file(w), None);
        assert_eq!(instance_specifier(w), "./add.wasm?vite-wasm-instance");
        assert!(is_declarable("$a_1") && !is_declarable("default") && !is_declarable("1a"));
    }

    #[test]
    fn reads_the_playground_modules() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/upstream/vite/playground/wasm");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "wasm") {
                let bytes = std::fs::read(&p).unwrap();
                parse(&bytes).unwrap_or_else(|err| panic!("{}: {err}", p.display()));
            }
        }
    }
}
