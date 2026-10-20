//! The JSX runtime module of the automatic runtime (`jsx: react-jsx` / `react-jsxdev`):
//! `<jsxImportSource>/jsx-runtime` (`/jsx-dev-runtime`), which every file containing JSX
//! imports implicitly. The source is the file's `@jsxImportSource` pragma, else the
//! `jsxImportSource` option, else `react`. The program loads the runtime like an import;
//! its `JSX` export (else the global `JSX`) is the file's JSX namespace.

use ts_r_ast::{Ast, Node, NodeId, Span};
use ts_r_config::{CompilerOptions, JsxEmit};

use crate::program::SourceFile;

/// The runtime module specifier `f` imports implicitly, if any (`f` contains JSX and uses
/// the automatic runtime).
pub fn runtime_specifier(f: &SourceFile, options: &CompilerOptions) -> Option<String> {
    let dev = file_runtime(options.jsx(), &f.ast, &f.text)?;
    if !f.kind.is_jsx() || !contains_jsx(&f.ast) {
        return None;
    }
    let e = options.effective();
    let source = import_source_pragma(&f.ast, &f.text).unwrap_or_else(|| e.jsx_import_source());
    Some(runtime_module_name(source, dev))
}

/// Whether a file uses the automatic runtime (`Some(dev)`): `jsx: react-jsx` /
/// `react-jsxdev`, `jsx: react` with `@jsxRuntime automatic`, or any mode with a
/// `@jsxImportSource` pragma (`jsx: preserve` too); the last `@jsxRuntime` pragma wins
/// (`classic` turns the automatic runtime off).
pub fn file_runtime(jsx: Option<JsxEmit>, ast: &Ast, text: &str) -> Option<bool> {
    let pragma = leading_pragma_at(ast, text, "@jsxRuntime", true);
    let jsx = jsx?;
    if pragma == Some("classic") {
        return None;
    }
    match jsx {
        JsxEmit::ReactJsx => Some(false),
        JsxEmit::ReactJsxDev => Some(true),
        JsxEmit::React if pragma == Some("automatic") => Some(false),
        _ if import_source_pragma(ast, text).is_some() => Some(false),
        _ => None,
    }
}

/// `<source>/jsx-runtime` or `<source>/jsx-dev-runtime`.
pub fn runtime_module_name(source: &str, dev: bool) -> String {
    let suffix = if dev {
        "jsx-dev-runtime"
    } else {
        "jsx-runtime"
    };
    format!("{source}/{suffix}")
}

/// Whether the file has a JSX element or fragment.
pub fn contains_jsx(ast: &Ast) -> bool {
    (0..ast.node_count() as u32).any(|i| {
        NodeId::from_raw(i).is_some_and(|n| {
            matches!(
                ast.node(n),
                Node::JsxElement { .. } | Node::JsxFragment { .. }
            )
        })
    })
}

/// The value of a `@jsxImportSource` pragma in the file's leading comments.
pub fn import_source_pragma<'t>(ast: &Ast, text: &'t str) -> Option<&'t str> {
    leading_pragma(ast, text, "@jsxImportSource")
}

/// The first value of pragma `tag` (`@jsx`, `@jsxImportSource`) in the comments before the
/// file's first token (tsc reads pragmas only there).
pub(crate) fn leading_pragma<'t>(ast: &Ast, text: &'t str, tag: &str) -> Option<&'t str> {
    leading_pragma_at(ast, text, tag, false)
}

/// The first (or `last`) value of pragma `tag` in the file's leading comments.
fn leading_pragma_at<'t>(ast: &Ast, text: &'t str, tag: &str, last: bool) -> Option<&'t str> {
    let mut end = if text.starts_with("#!") {
        text.find('\n').unwrap_or(text.len())
    } else {
        0
    };
    let mut found = None;
    for c in ast.comments_in(Span::new(0, text.len() as u32)) {
        let (start, stop) = (c.span.start as usize, c.span.end as usize);
        if !text.get(end..start)?.trim().is_empty() {
            break;
        }
        end = stop;
        // Pragma names are matched without regard to case (`@jsxfrag`).
        let raw = text.get(start..stop)?;
        let lower = raw.to_ascii_lowercase();
        let tag_l = tag.to_ascii_lowercase();
        let mut from = 0;
        while let Some(i) = lower.get(from..).and_then(|l| l.find(&tag_l)) {
            from += i + tag.len();
            let rest = raw.get(from..)?;
            if rest.starts_with(char::is_whitespace) {
                let value = rest.trim_start();
                let len = value
                    .find(|ch: char| ch.is_whitespace() || ch == '*')
                    .unwrap_or(value.len());
                if len > 0 {
                    if !last {
                        return Some(&value[..len]);
                    }
                    found = Some(&value[..len]);
                }
            }
        }
    }
    found
}
