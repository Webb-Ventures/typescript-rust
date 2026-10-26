//! `import/no-restricted-paths`: imports from a zone's `from` paths in files under its
//! `target` paths.
//!
//! - `target`, `from` and `except` are resolved against `basePath` (default: the cwd); a
//!   path with glob characters matches as a glob, any other as a directory prefix.
//! - A zone whose `from` mixes globs and plain paths reports "must contain either only
//!   glob patterns or none" for every resolved import.
//! - With plain `from` paths, every `except` must lie inside the `from` directory, and
//!   with glob `from` paths every `except` must be a glob; otherwise each import from the
//!   zone reports the invalid exception instead.
//! - An import resolving into a `from` path (and not an exception) reports "Unexpected path
//!   ... imported in restricted zone.", plus the zone's `message`, once per matching
//!   `from` path.

use super::import_util::{MODULE_KINDS, ModuleSystems, VisitorOptions, resolve};
use super::prelude::*;
use crate::config::glob::Glob;
use crate::modules::import_type::{ImportType, import_type};
use crate::modules::relative;
use ts_r_config::path;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-restricted-paths",
        "Enforce which files can be imported in a given folder.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoRestrictedPaths>,
};

#[derive(Debug)]
struct Zone {
    target: Vec<String>,
    from: Vec<String>,
    except: Vec<String>,
    message: Option<String>,
}

#[derive(Debug)]
struct NoRestrictedPaths {
    zones: Vec<Zone>,
    base_path: Option<String>,
}

fn one_or_many(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

impl FromOptions for NoRestrictedPaths {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let zones = o
            .get("zones")
            .and_then(Value::as_array)
            .map(|zs| {
                zs.iter()
                    .map(|z| Zone {
                        target: one_or_many(&z["target"]),
                        from: one_or_many(&z["from"]),
                        except: one_or_many(&z["except"]),
                        message: z["message"]
                            .as_str()
                            .filter(|m| !m.is_empty())
                            .map(str::to_owned),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(NoRestrictedPaths {
            zones,
            base_path: o
                .get("basePath")
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty())
                .map(str::to_owned),
        })
    }
}

/// is-glob's strict check: `*`, a `[...]` class, a `{...}` group, an extglob `(a|b)`, a
/// leading `!`, or `?` after `]`, `.`, `+` or `)`.
fn is_glob(s: &str) -> bool {
    let b = s.as_bytes();
    if b.first() == Some(&b'!') {
        return true;
    }
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'*' => return true,
            b'?' if i > 0 && matches!(b[i - 1], b']' | b'.' | b'+' | b')') => return true,
            b'[' => {
                if let Some(end) = b[i + 1..].iter().position(|&c| c == b']')
                    && end > 0
                    && !b[i + 1..i + 1 + end].contains(&b'\\')
                {
                    return true;
                }
            }
            b'{' => {
                if let Some(end) = b[i + 1..].iter().position(|&c| c == b'}')
                    && end > 0
                    && !b[i + 1..i + 1 + end].contains(&b'\\')
                {
                    return true;
                }
            }
            b'(' => {
                if let Some(end) = b[i + 1..].iter().position(|&c| c == b')') {
                    let inner = &b[i + 1..i + 1 + end];
                    if inner.contains(&b'|') || matches!(inner.first(), Some(b'?')) {
                        return true;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// `filepath` is `target` or inside it.
fn contains_path(filepath: &str, target: &str) -> bool {
    let rel = relative(target, filepath);
    rel.is_empty() || !rel.starts_with("..")
}

fn matches_target(file: &str, target: &str) -> bool {
    if is_glob(target) {
        Glob::new(target).matches(file)
    } else {
        contains_path(file, target)
    }
}

enum Validator {
    Mixed,
    Glob {
        from: String,
        except: Option<Vec<String>>,
    },
    Plain {
        from: String,
        except: Option<Vec<String>>,
    },
}

const MIXED: &str = "Restricted path `from` must contain either only glob patterns or none";
const BAD_GLOB_EXCEPTION: &str =
    "Restricted path exceptions must be glob patterns when `from` contains glob patterns";
const BAD_EXCEPTION: &str =
    "Restricted path exceptions must be descendants of the configured `from` path for that zone.";

impl Validator {
    fn restricts(&self, p: &str) -> bool {
        match self {
            Validator::Mixed => true,
            Validator::Glob { from, .. } => Glob::new(from).matches(p),
            Validator::Plain { from, .. } => contains_path(p, from),
        }
    }

    /// `Err(message)` when the exceptions are invalid, else whether `p` is an exception.
    fn exception(&self, p: &str) -> Result<bool, &'static str> {
        match self {
            Validator::Mixed => Err(MIXED),
            Validator::Glob { except: None, .. } => Err(BAD_GLOB_EXCEPTION),
            Validator::Plain { except: None, .. } => Err(BAD_EXCEPTION),
            Validator::Glob {
                except: Some(e), ..
            } => Ok(e.iter().any(|g| Glob::new(g).matches(p))),
            Validator::Plain {
                except: Some(e), ..
            } => Ok(e.iter().any(|x| contains_path(p, x))),
        }
    }
}

impl NoRestrictedPaths {
    fn validators(&self, cx: &mut Ctx<'_, '_>, zone: &Zone, base: &str) -> Vec<Validator> {
        let globs: Vec<bool> = zone.from.iter().map(|f| is_glob(f)).collect();
        if globs.iter().any(|g| *g) && globs.iter().any(|g| !*g) {
            return vec![Validator::Mixed];
        }
        let all_glob = globs.iter().all(|g| *g);
        let mut out = Vec::new();
        for f in &zone.from {
            let from = path::resolve(base, f);
            if all_glob {
                let except = zone
                    .except
                    .iter()
                    .all(|e| is_glob(e))
                    .then(|| zone.except.clone());
                out.push(Validator::Glob { from, except });
            } else {
                let abs: Vec<String> = zone
                    .except
                    .iter()
                    .map(|e| path::resolve(&from, e))
                    .collect();
                let mut valid = true;
                for e in &abs {
                    let rel = relative(&from, e);
                    let resolved = resolve(cx, &rel);
                    let m = cx.file.modules();
                    if import_type(m, &rel, resolved.as_ref().and_then(|r| r.path()))
                        == ImportType::Parent
                    {
                        valid = false;
                    }
                }
                out.push(Validator::Plain {
                    from,
                    except: valid.then_some(abs),
                });
            }
        }
        out
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, source: NodeId, value: &str) {
        let Some(resolved) = resolve(cx, value) else {
            return;
        };
        let Some(target_path) = resolved.path().map(str::to_owned) else {
            return;
        };
        let m = cx.file.modules();
        let base = self.base_path.clone().unwrap_or_else(|| m.cwd.clone());
        let file = m.file.clone();
        let span = cx.span(source);
        for zone in &self.zones {
            let matching = zone
                .target
                .iter()
                .any(|t| matches_target(&file, &path::resolve(&base, t)));
            if !matching {
                continue;
            }
            let validators = self.validators(cx, zone, &base);
            let applicable: Vec<&Validator> = validators
                .iter()
                .filter(|v| v.restricts(&target_path))
                .collect();
            for v in &applicable {
                if let Err(message) = v.exception(&target_path) {
                    cx.report(span, message);
                }
            }
            for v in &applicable {
                if v.exception(&target_path) == Ok(false) {
                    let extra = zone
                        .message
                        .as_deref()
                        .map(|m| format!(" {m}"))
                        .unwrap_or_default();
                    cx.report(
                        span,
                        format!("Unexpected path \"{value}\" imported in restricted zone.{extra}"),
                    );
                }
            }
        }
    }
}

impl Rule for NoRestrictedPaths {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.zones.is_empty() {
            return;
        }
        let visitor = VisitorOptions {
            systems: ModuleSystems {
                esmodule: true,
                commonjs: true,
                amd: false,
            },
            ignore: Vec::new(),
        };
        for (r, value) in visitor.refs(cx.ast(), node) {
            self.check(cx, r.source, value);
        }
    }
}
