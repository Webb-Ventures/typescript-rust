//! `cargo xtask codegen lint-globals`: the global names each TypeScript lib file declares, for
//! ts-r-lint's `no-undef` / `builtinGlobals` on TypeScript code.
//!
//! Input: the pinned tsc 7 lib files (`tsc/internal/bundled/libs/lib.*.d.ts` in the synced
//! TypeScript tree). Each file is parsed with `ts_r_parser`; its top-level declarations are
//! split into names with a type meaning and names with a value meaning, and its
//! `/// <reference lib="..." />` directives are kept so the lint crate can follow them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use ts_r_parser::ts_r_ast::{Ast, ModuleKind, Node, NodeId, SourceKind};

use crate::util::Result;

/// Where the bundled lib files live inside the synced TypeScript tree.
pub const LIBS_DIR: &str = "tsc/internal/bundled/libs";

/// Default output, relative to the workspace root (checked in).
pub const DEFAULT_OUT: &str = "crates/ts_r_lint/src/lib_globals_generated.rs";

/// One lib file's references and declared names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LibNames {
    pub references: Vec<String>,
    pub types: BTreeSet<String>,
    pub values: BTreeSet<String>,
}

/// Reads every `lib.*.d.ts` (and `lib.d.ts`) under `dir`, keyed by file name.
pub fn read_libs(dir: &Path) -> Result<BTreeMap<String, LibNames>> {
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("lib.") && name.ends_with(".d.ts")) {
            continue;
        }
        let src = std::fs::read_to_string(e.path())?;
        let names = lib_names(&name, &src)?;
        out.insert(name, names);
    }
    if out.is_empty() {
        return Err(format!("no lib.*.d.ts files in {}", dir.display()).into());
    }
    for (file, n) in &out {
        if let Some(r) = n.references.iter().find(|r| !out.contains_key(*r)) {
            return Err(format!("{file}: <reference lib> to missing file {r}").into());
        }
    }
    Ok(out)
}

/// The references and top-level names of one lib file.
pub fn lib_names(file: &str, src: &str) -> Result<LibNames> {
    let parsed = ts_r_parser::parse(src, SourceKind::Dts);
    if let Some(d) = parsed.diagnostics.first() {
        return Err(format!(
            "{file}: parse error at byte {}: {}",
            d.span.start, d.message
        )
        .into());
    }
    let ast = &parsed.ast;
    let mut out = LibNames {
        references: references(src),
        ..LibNames::default()
    };
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return Ok(out);
    };
    for &stmt in ast.list(stmts) {
        declare(ast, stmt, &mut out);
    }
    Ok(out)
}

fn ident(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::QualifiedName { left, .. } => ident(ast, left),
        _ => None,
    }
}

fn declare(ast: &Ast, stmt: NodeId, out: &mut LibNames) {
    let (name, ty, value) = match *ast.node(stmt) {
        Node::Interface { name, .. } | Node::TypeAlias { name, .. } => (Some(name), true, false),
        Node::ClassDecl { name, .. } => (name, true, true),
        Node::Enum { name, .. } => (Some(name), true, true),
        // typescript-eslint's scope analysis defines a namespace as both a type and a value.
        Node::Module { kind, name, .. } if kind != ModuleKind::Global => (Some(name), true, true),
        Node::FunctionDecl { name, .. } => (name, false, true),
        Node::VarDecl { decls, .. } => {
            for &d in ast.list(decls) {
                if let Node::VarDeclarator { name, .. } = *ast.node(d)
                    && let Some(n) = ident(ast, name)
                {
                    out.values.insert(n);
                }
            }
            return;
        }
        _ => return,
    };
    let Some(n) = name.and_then(|n| ident(ast, n)) else {
        return;
    };
    if ty {
        out.types.insert(n.clone());
    }
    if value {
        out.values.insert(n);
    }
}

/// `/// <reference lib="x" />` directives, as file names (`lib.x.d.ts`), in source order.
fn references(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("///") else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with("<reference") {
            continue;
        }
        // ` lib=` (not `no-default-lib=`).
        let Some(i) = rest.find(" lib=").map(|i| i + 1) else {
            continue;
        };
        let v = &rest[i + 4..];
        let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        if let Some(end) = v[1..].find(q) {
            out.push(format!("lib.{}.d.ts", v[1..1 + end].to_ascii_lowercase()));
        }
    }
    out
}

/// Renders the generated module. Output depends only on the inputs.
pub fn render(libs: &BTreeMap<String, LibNames>, commit: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "// @generated by `cargo xtask codegen lint-globals` from microsoft/TypeScript@{commit}\n\
         // ({LIBS_DIR}/lib.*.d.ts). Do not edit by hand.\n\
         // Names: Copyright (c) Microsoft Corporation, Apache-2.0.\n"
    );
    s.push_str(
        "/// The global names one TypeScript lib file declares at the top level.\n\
         #[derive(Debug)]\n\
         pub struct LibFile {\n    \
         /// File name, e.g. `lib.es2015.core.d.ts`.\n    \
         pub file: &'static str,\n    \
         /// `/// <reference lib=\"...\" />` targets as file names, in source order.\n    \
         pub references: &'static [&'static str],\n    \
         /// Names with a type meaning (interfaces, type aliases, classes, enums, namespaces), sorted.\n    \
         pub types: &'static [&'static str],\n    \
         /// Names with a value meaning (variables, functions, classes, enums, namespaces), sorted.\n    \
         pub values: &'static [&'static str],\n\
         }\n\n\
         /// Every lib file, sorted by file name.\n\
         pub static LIB_FILES: &[LibFile] = &[\n",
    );
    for (file, n) in libs {
        let _ = writeln!(s, "    LibFile {{\n        file: {file:?},");
        list(&mut s, "references", n.references.iter());
        list(&mut s, "types", n.types.iter());
        list(&mut s, "values", n.values.iter());
        s.push_str("    },\n");
    }
    s.push_str("];\n");
    s
}

/// `        field: &["a", "b", ...],` wrapped at 100 columns.
fn list<'a>(s: &mut String, field: &str, items: impl Iterator<Item = &'a String>) {
    let items: Vec<String> = items.map(|i| format!("{i:?}")).collect();
    if items.is_empty() {
        let _ = writeln!(s, "        {field}: &[],");
        return;
    }
    let _ = writeln!(s, "        {field}: &[");
    let mut line = String::new();
    for it in items {
        if !line.is_empty() && 12 + line.len() + it.len() + 2 > 100 {
            let _ = writeln!(s, "            {}", line.trim_end());
            line.clear();
        }
        line.push_str(&it);
        line.push_str(", ");
    }
    if !line.is_empty() {
        let _ = writeln!(s, "            {}", line.trim_end());
    }
    s.push_str("        ],\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_by_meaning() {
        let src = "/// <reference lib=\"es2015.core\" />\n\
                   /// <reference no-default-lib=\"true\"/>\n\
                   interface A { x: { y: 1 } }\n\
                   declare var A: { new(): A };\n\
                   type B = string;\n\
                   declare var c: number, d: string;\n\
                   declare function e(): void;\n\
                   declare namespace F.G { var h: number; }\n\
                   declare class K {}\n";
        let n = lib_names("lib.test.d.ts", src).unwrap();
        assert_eq!(n.references, vec!["lib.es2015.core.d.ts"]);
        let t: Vec<_> = n.types.iter().map(String::as_str).collect();
        let v: Vec<_> = n.values.iter().map(String::as_str).collect();
        assert_eq!(t, ["A", "B", "F", "K"]);
        assert_eq!(v, ["A", "F", "K", "c", "d", "e"]);
    }

    #[test]
    fn render_is_stable() {
        let mut libs = BTreeMap::new();
        libs.insert(
            "lib.x.d.ts".to_owned(),
            lib_names("lib.x.d.ts", "interface A {}\ndeclare var b: A;").unwrap(),
        );
        let a = render(&libs, "abc");
        assert_eq!(a, render(&libs, "abc"));
        assert!(a.contains("file: \"lib.x.d.ts\""));
        assert!(a.contains("types: &[\n            \"A\",\n        ],"));
        assert!(a.contains("references: &[],"));
    }
}
