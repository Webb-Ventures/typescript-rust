//! The kind of a module specifier (`import/order` groups and friends), from its text and
//! where it resolves, first match wins:
//!
//! 1. `internal` when it matches `import/internal-regex`;
//! 2. `absolute` for an absolute path;
//! 3. `builtin` when it did not resolve to a file and its package part is a Node builtin
//!    or in `import/core-modules`;
//! 4. `parent` (`..`, `../x`), `index` (`.`, `./`, `./index`, `./index.js`), `sibling`
//!    (`./x`);
//! 5. `external` when the file lies outside the linted file's package directory or under
//!    one of its `import/external-module-folders` (`node_modules`);
//! 6. `internal` when the file lies inside the package directory;
//! 7. `external` for a name that starts with a word character or a scope, else `unknown`.

use super::{ModCtx, is_core_module, relative};
use ts_r_config::path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportType {
    Internal,
    Absolute,
    Builtin,
    Parent,
    Index,
    Sibling,
    External,
    Unknown,
}

/// `@scope/name...` (`/^@[^/]+\/?[^/]+/`).
pub(crate) fn is_scoped(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('@') else {
        return false;
    };
    let scope_len = rest.find('/').unwrap_or(rest.len());
    if scope_len == 0 {
        return false;
    }
    // `[^/]+\/?[^/]+`: at least two non-slash characters in all, around an optional slash.
    let after = &rest[scope_len..];
    match after.strip_prefix('/') {
        Some(pkg) => !pkg.is_empty() && !pkg.starts_with('/'),
        None => rest.chars().count() >= 2,
    }
}

/// Starts with a word character (`/^\w/`).
pub(crate) fn is_module(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The package part of a name: `@scope/name` or the first segment.
pub(crate) fn base_module(name: &str) -> String {
    if is_scoped(name) {
        let mut parts = name.split('/');
        let scope = parts.next().unwrap_or("");
        let pkg = parts.next().unwrap_or("");
        format!("{scope}/{pkg}")
    } else {
        name.split('/').next().unwrap_or("").to_owned()
    }
}

pub(crate) fn is_builtin(cx: &ModCtx, name: &str, resolved: Option<&str>) -> bool {
    if resolved.is_some() || name.is_empty() {
        return false;
    }
    let base = base_module(name);
    is_core_module(&base) || cx.settings.core_modules.contains(&base)
}

fn is_relative_to_parent(name: &str) -> bool {
    name == ".." || name.starts_with("../") || name.starts_with("..\\")
}

fn is_index(name: &str) -> bool {
    matches!(name, "." | "./" | "./index" | "./index.js")
}

fn is_relative_to_sibling(name: &str) -> bool {
    name.starts_with("./") || name.starts_with(".\\")
}

pub(crate) fn is_external_path(cx: &ModCtx, resolved: Option<&str>) -> bool {
    let Some(p) = resolved else {
        return false;
    };
    let pkg = cx.package_path();
    if relative(&pkg, p).starts_with("..") {
        return true;
    }
    cx.settings.external_folders.iter().any(|folder| {
        let folder_path = path::resolve(&pkg, folder);
        !relative(&folder_path, p).starts_with("..")
    })
}

fn is_internal_path(cx: &ModCtx, resolved: Option<&str>) -> bool {
    let Some(p) = resolved else {
        return false;
    };
    !relative(&cx.package_path(), p).starts_with("../")
}

fn internal_regex_match(cx: &ModCtx, name: &str) -> bool {
    cx.settings
        .internal_regex
        .as_deref()
        .and_then(crate::regex::compile)
        .is_some_and(|r| r.is_match(name, false))
}

/// The type of `name`, which resolved to `resolved` (`None`: a builtin or unresolved).
pub(crate) fn import_type(cx: &ModCtx, name: &str, resolved: Option<&str>) -> ImportType {
    if internal_regex_match(cx, name) {
        ImportType::Internal
    } else if name.starts_with('/') {
        ImportType::Absolute
    } else if is_builtin(cx, name, resolved) {
        ImportType::Builtin
    } else if is_relative_to_parent(name) {
        ImportType::Parent
    } else if is_index(name) {
        ImportType::Index
    } else if is_relative_to_sibling(name) {
        ImportType::Sibling
    } else if is_external_path(cx, resolved) {
        ImportType::External
    } else if is_internal_path(cx, resolved) {
        ImportType::Internal
    } else if is_module(name) || is_scoped(name) {
        ImportType::External
    } else {
        ImportType::Unknown
    }
}
