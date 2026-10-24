//! One file: TypeScript/TSX/JSX → JavaScript Node can run, plus the module specifiers the
//! output imports (so the graph walk and the resolve hook know what to resolve).

use std::io::IsTerminal as _;

use ts_r_ast::{Ast, Modifiers, Node, NodeId, SourceKind};
use ts_r_codegen::{CodegenOptions, Layout, PrintHints, SourceMapOptions};
use ts_r_diagnostics::{Renderer, format_pretty};
use ts_r_span::SourceText;
use ts_r_transform::{ModuleFormat, TransformOptions};

/// The module system a compiled file runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Esm,
    Cjs,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Esm => "esm",
            Format::Cjs => "cjs",
        }
    }
}

/// The `type` of the nearest `package.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackageType {
    Module,
    CommonJs,
    /// No `package.json`, or no `type` field: Node detects the format from the syntax.
    Unspecified,
}

/// A module specifier in compiled output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub text: String,
    /// Loaded with `require()` (CommonJS resolution and conditions) rather than `import`.
    pub require: bool,
}

/// A compiled file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compiled {
    pub format: Format,
    /// JavaScript, with an inline source map comment when maps are on.
    pub code: String,
    pub specs: Vec<Spec>,
}

/// Files ts-r-run compiles: TypeScript sources (not declarations) and JSX.
pub fn is_compiled_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(ext, "ts" | "tsx" | "mts" | "cts" | "jsx")
        && SourceKind::from_path(&lower) != SourceKind::Dts
}

/// The format of `path` given its package type: `.mts` → ESM, `.cts` → CommonJS, else the
/// `package.json` `type`, else Node's syntax detection (ES module syntax → ESM). Like tsx, a
/// file in a `"type": "module"` package that has no ES module syntax and assigns CommonJS
/// exports runs as CommonJS (`require` alone works in ES output, through `createRequire`).
fn format_of(path: &str, pkg: PackageType, ast: &Ast, src: &str) -> Format {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".mts") || lower.ends_with(".mjs") {
        return Format::Esm;
    }
    if lower.ends_with(".cts") || lower.ends_with(".cjs") {
        return Format::Cjs;
    }
    match pkg {
        PackageType::Module => {
            if assigns_cjs_exports(src) && !has_module_syntax(ast, src) {
                Format::Cjs
            } else {
                Format::Esm
            }
        }
        PackageType::CommonJs => Format::Cjs,
        PackageType::Unspecified => {
            if has_esm_syntax(ast, src) {
                Format::Esm
            } else {
                Format::Cjs
            }
        }
    }
}

/// Whether the file is an ES module by syntax once types are erased: a value
/// `import`/`export` (type-only declarations don't count), `import.meta` or top-level
/// `await`. Like tsx, a file that has ES module syntax but also uses CommonJS globals
/// (`require`, `module`, `exports`, `__dirname`, `__filename`) and needs nothing ESM-only
/// runs as CommonJS, so the two can be mixed in packages without a `type`.
pub fn has_esm_syntax(ast: &Ast, src: &str) -> bool {
    if src.contains("import.meta") || has_top_level_await(ast) {
        return true;
    }
    has_value_module_decl(ast) && !uses_cjs_globals(src)
}

/// Any ES module syntax: a value `import`/`export`, `import.meta` or top-level `await`.
fn has_module_syntax(ast: &Ast, src: &str) -> bool {
    src.contains("import.meta") || has_value_module_decl(ast) || has_top_level_await(ast)
}

/// A value `import` or `export` declaration at the top level.
fn has_value_module_decl(ast: &Ast) -> bool {
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return false;
    };
    for &s in ast.list(stmts) {
        let n = *ast.node(s);
        let esm = match n {
            Node::ImportDecl { type_only, .. }
            | Node::ExportNamed { type_only, .. }
            | Node::ExportAll { type_only, .. } => !type_only,
            Node::ExportDefaultExpr { .. } => true,
            Node::Interface { .. } | Node::TypeAlias { .. } | Node::ImportEquals { .. } => false,
            _ => {
                let m = n.modifiers();
                m.contains(Modifiers::EXPORT) && !m.contains(Modifiers::DECLARE)
            }
        };
        if esm {
            return true;
        }
    }
    false
}

fn assigns_cjs_exports(src: &str) -> bool {
    src.contains("module.exports") || src.contains("exports.")
}

fn uses_cjs_globals(src: &str) -> bool {
    [
        "require(",
        "module.exports",
        "exports.",
        "__dirname",
        "__filename",
    ]
    .iter()
    .any(|p| src.contains(p))
}

/// `await` or `for await` outside any function (and so needing an ES module).
fn has_top_level_await(ast: &Ast) -> bool {
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        match *ast.node(id) {
            Node::Await { .. } | Node::ForOf { is_await: true, .. } => return true,
            Node::VarDecl {
                kind: ts_r_ast::VarKind::AwaitUsing,
                ..
            } => return true,
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodDef { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => continue,
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// Specifiers of `import`/`export ... from`, `import("literal")` and `require("literal")` in
/// the transformed tree.
fn collect_specs(ast: &Ast) -> Vec<Spec> {
    let mut out: Vec<Spec> = Vec::new();
    let mut stack: Vec<NodeId> = vec![ast.root];
    let lit = |id: NodeId| match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    };
    while let Some(id) = stack.pop() {
        match *ast.node(id) {
            Node::ImportDecl { source, .. }
            | Node::ExportAll { source, .. }
            | Node::ExportNamed {
                source: Some(source),
                ..
            } => {
                if let Some(text) = lit(source) {
                    out.push(Spec {
                        text,
                        require: false,
                    });
                }
                continue;
            }
            Node::ImportCall { arg, .. } => {
                if let Some(text) = lit(arg) {
                    out.push(Spec {
                        text,
                        require: false,
                    });
                }
            }
            Node::Call { callee, args, .. } => {
                if let Node::Ident { name } = *ast.node(callee)
                    && matches!(ast.atom(name), "require" | "__require")
                    && let Some(&first) = ast.list(args).first()
                    && let Some(text) = lit(first)
                {
                    out.push(Spec {
                        text,
                        require: true,
                    });
                }
            }
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    // Source order (the stack visits children last-first), without duplicates.
    out.reverse();
    let mut seen = rustc_hash::FxHashSet::default();
    out.retain(|s| seen.insert((s.text.clone(), s.require)));
    out
}

/// The options one file is transformed with: the project's options with the file's module
/// format.
pub fn file_options(base: &TransformOptions, format: Format) -> TransformOptions {
    let mut o = base.clone();
    o.module = match format {
        Format::Esm => ModuleFormat::Esm { node_require: true },
        Format::Cjs => ModuleFormat::CommonJs,
    };
    o
}

/// Compiles `src`. Errors are syntax errors, rendered tsc-style.
pub fn compile(
    path: &str,
    src: &str,
    base: &TransformOptions,
    pkg: PackageType,
    source_maps: bool,
) -> Result<Compiled, String> {
    let kind = SourceKind::from_path(path);
    let parsed = ts_r_parser::parse(src, kind);
    // Only the errors a build fails on (as esbuild): tsc's checker grammar errors such as
    // `if (x) ;` or `declare function f() {}` are not syntax errors here.
    let errors = ts_r_transform::build_errors(&parsed);
    if !errors.is_empty() {
        return Err(render_errors(path, src, &errors));
    }
    let mut ast = parsed.ast;
    let format = format_of(path, pkg, &ast, src);
    let opts = file_options(base, format);
    let result = ts_r_transform::transform(&mut ast, src, &opts);
    let specs = collect_specs(&ast);
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
    let codegen = CodegenOptions {
        layout: Layout::Readable,
        comments: false,
        source_map: source_maps.then_some(SourceMapOptions {
            file: None,
            // Relative to the module's own URL, so cached output has no absolute paths.
            source: name,
            source_root: None,
            include_content: false,
        }),
        indent_width: 2,
        newline: "\n",
        collect_specifiers: false,
    };
    let hints = PrintHints {
        erased: &result.erased,
        prelude: &result.prelude,
        ..Default::default()
    };
    let printed = ts_r_codegen::print_with(&ast, src, &codegen, &hints);
    let mut code = printed.code;
    if let Some(map) = printed.map {
        if !code.ends_with('\n') {
            code.push('\n');
        }
        code.push_str("//# sourceMappingURL=");
        code.push_str(&map.to_data_url());
        code.push('\n');
    }
    Ok(Compiled {
        format,
        code,
        specs,
    })
}

fn render_errors(path: &str, src: &str, diags: &[&ts_r_diagnostics::Diagnostic]) -> String {
    let file = SourceText::new(path, src);
    let r = Renderer {
        color: std::io::stderr().is_terminal(),
    };
    let mut out = String::new();
    for d in diags {
        out.push_str(&format_pretty(d, &file, r));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

// ---- cache payload: [format u8][spec count u32]{[require u8][len u32][utf8]}[code] ----

pub fn encode(c: &Compiled) -> Vec<u8> {
    let mut out = Vec::with_capacity(c.code.len() + 16 + c.specs.len() * 24);
    out.push(match c.format {
        Format::Esm => 0,
        Format::Cjs => 1,
    });
    out.extend_from_slice(&(c.specs.len() as u32).to_le_bytes());
    for s in &c.specs {
        out.push(u8::from(s.require));
        out.extend_from_slice(&(s.text.len() as u32).to_le_bytes());
        out.extend_from_slice(s.text.as_bytes());
    }
    out.extend_from_slice(c.code.as_bytes());
    out
}

pub fn decode(data: &[u8]) -> Option<Compiled> {
    fn u32_at(d: &[u8], at: usize) -> Option<usize> {
        let b = d.get(at..at + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }
    let format = match *data.first()? {
        0 => Format::Esm,
        1 => Format::Cjs,
        _ => return None,
    };
    let n = u32_at(data, 1)?;
    let mut at = 5;
    let mut specs = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let require = *data.get(at)? != 0;
        let len = u32_at(data, at + 1)?;
        let text = std::str::from_utf8(data.get(at + 5..at + 5 + len)?).ok()?;
        specs.push(Spec {
            text: text.to_owned(),
            require,
        });
        at += 5 + len;
    }
    let code = std::str::from_utf8(data.get(at..)?).ok()?.to_owned();
    Some(Compiled {
        format,
        code,
        specs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, src: &str, pkg: PackageType) -> Compiled {
        compile(path, src, &TransformOptions::default(), pkg, false).expect("compiles")
    }

    #[test]
    fn detects_format_like_node() {
        let esm = "import { a } from './a';\nexport const x: number = a;\n";
        let cjs =
            "import type { T } from './t';\nexport interface I {}\nconst x: T = require('./a');\n";
        assert_eq!(
            run("a.ts", esm, PackageType::Unspecified).format,
            Format::Esm
        );
        assert_eq!(
            run("a.ts", cjs, PackageType::Unspecified).format,
            Format::Cjs
        );
        assert_eq!(run("a.ts", cjs, PackageType::Module).format, Format::Esm);
        let exports = "import path = require('node:path');\nmodule.exports = { b: path.sep };\n";
        assert_eq!(
            run("a.ts", exports, PackageType::Module).format,
            Format::Cjs
        );
        let both = "export const a = 1;\nmodule.exports.b = 2;\n";
        assert_eq!(run("a.ts", both, PackageType::Module).format, Format::Esm);
        assert_eq!(run("a.ts", esm, PackageType::CommonJs).format, Format::Cjs);
        assert_eq!(run("a.mts", cjs, PackageType::CommonJs).format, Format::Esm);
        assert_eq!(run("a.cts", esm, PackageType::Module).format, Format::Cjs);
        let meta = "const u: string = import.meta.url;\n";
        assert_eq!(
            run("a.ts", meta, PackageType::Unspecified).format,
            Format::Esm
        );
        let tla =
            "const r: number = await Promise.resolve(1);\nconst f = async () => { await 0; };\n";
        assert_eq!(
            run("a.ts", tla, PackageType::Unspecified).format,
            Format::Esm
        );
        let nested = "const f = async () => { await 0; };\n";
        assert_eq!(
            run("a.ts", nested, PackageType::Unspecified).format,
            Format::Cjs
        );
        let mixed =
            "import { a } from './a';\nconst b = require('./b');\nexport const c = a + b;\n";
        assert_eq!(
            run("a.ts", mixed, PackageType::Unspecified).format,
            Format::Cjs
        );
    }

    #[test]
    fn collects_runtime_specifiers_only() {
        let src = "import type { T } from './types';\nimport { a } from './a.js';\nimport { U } from './only-types';\nexport * from './b';\nconst c = await import('./c');\nlet u: U; let t: T;\nconsole.log(a, c);\n";
        let out = run("m.ts", src, PackageType::Module);
        let texts: Vec<&str> = out.specs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["./a.js", "./b", "./c"]);
        assert!(out.specs.iter().all(|s| !s.require));
        let cjs = run("m.ts", src, PackageType::CommonJs);
        let texts: Vec<&str> = cjs.specs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["./a.js", "./b", "./c"]);
        assert!(cjs.specs.iter().all(|s| s.require));
    }

    #[test]
    fn payload_round_trips() {
        let c = run(
            "m.ts",
            "import x from 'x';\nexport const y = x as number;\n",
            PackageType::Module,
        );
        assert_eq!(decode(&encode(&c)), Some(c));
        assert_eq!(decode(&[7]), None);
        assert_eq!(decode(&[0, 9, 0, 0, 0]), None);
    }

    #[test]
    fn syntax_errors_are_reported() {
        let e = compile(
            "bad.ts",
            "let x = ;\n",
            &TransformOptions::default(),
            PackageType::Module,
            false,
        )
        .expect_err("syntax error");
        assert!(e.contains("bad.ts"), "{e}");
    }

    #[test]
    fn grammar_errors_esbuild_accepts_do_not_stop_a_run() {
        let src = "declare function f() { return 1 }\nif (globalThis.x) ;\nconsole.log(1);\n";
        let c = run("a.ts", src, PackageType::Module);
        assert!(c.code.contains("console.log(1)"), "{}", c.code);
        let e = compile(
            "b.ts",
            "const x: number;\n",
            &TransformOptions::default(),
            PackageType::Module,
            false,
        )
        .expect_err("early error");
        assert!(e.contains("must be initialized"), "{e}");
    }

    #[test]
    fn inline_source_map_uses_the_file_name() {
        let c = compile(
            "/abs/dir/m.ts",
            "const a: number = 1;\nthrow new Error(String(a));\n",
            &TransformOptions::default(),
            PackageType::Module,
            true,
        )
        .expect("compiles");
        assert!(
            c.code
                .contains("//# sourceMappingURL=data:application/json;base64,")
        );
        assert!(!c.code.contains("/abs/dir"));
    }

    #[test]
    fn compiled_paths() {
        for p in ["a.ts", "b.TSX", "c.mts", "d.cts", "e.jsx"] {
            assert!(is_compiled_path(p), "{p}");
        }
        for p in ["a.d.ts", "b.d.mts", "c.js", "d.json", "e.d.css.ts"] {
            assert!(!is_compiled_path(p), "{p}");
        }
    }
}
