//! `tsc --showConfig` output.
//!
//! Reproduces tsc 7's text exactly (4-space indented JSON, no trailing newline): explicit
//! options in declaration order, then implied options whose computed value differs from
//! the default, then `references`, `files`, `include` and `exclude`.

use crate::enums::{ModuleDetection, ModuleKind, ModuleResolution, lib_canonical_name};
use crate::jsonc;
use crate::options::{CompilerOptions, OptionId, OptionType, OptionValue, flags};
use crate::path;
use crate::tsconfig::ParsedConfig;

/// A tiny JSON tree for pretty printing.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `true`/`false`
    Bool(bool),
    /// A number.
    Number(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object with ordered keys.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Pretty-prints with 4-space indentation like tsc.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        write_pretty(self, 0, &mut out);
        out
    }
}

fn write_pretty(j: &Json, indent: usize, out: &mut String) {
    match j {
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(&jsonc::format_number(*n)),
        Json::String(s) => jsonc::write_json_string(s, out),
        Json::Array(items) if items.is_empty() => out.push_str("[]"),
        Json::Object(items) if items.is_empty() => out.push_str("{}"),
        Json::Array(items) => {
            out.push_str("[\n");
            for (i, it) in items.iter().enumerate() {
                push_indent(indent + 1, out);
                write_pretty(it, indent + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push(']');
        }
        Json::Object(items) => {
            out.push_str("{\n");
            for (i, (k, v)) in items.iter().enumerate() {
                push_indent(indent + 1, out);
                jsonc::write_json_string(k, out);
                out.push_str(": ");
                write_pretty(v, indent + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push('}');
        }
    }
}

fn push_indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("    ");
    }
}

fn rel(cfg: &ParsedConfig, p: &str) -> String {
    path::relative_dotted(&cfg.config_dir, p, cfg.case_sensitive())
}

fn option_json(cfg: &ParsedConfig, id: OptionId, v: &OptionValue) -> Option<Json> {
    let ty = id.decl().ty;
    Some(match v {
        OptionValue::Bool(b) => Json::Bool(*b),
        OptionValue::Number(n) => Json::Number(*n),
        OptionValue::String(s) => {
            if ty == OptionType::Path {
                Json::String(rel(cfg, s))
            } else {
                Json::String(s.clone())
            }
        }
        OptionValue::List(items) => Json::Array(
            items
                .iter()
                .map(|s| {
                    Json::String(match ty {
                        OptionType::PathList => rel(cfg, s),
                        OptionType::Lib => lib_canonical_name(s).unwrap_or(s).to_owned(),
                        _ => s.clone(),
                    })
                })
                .collect(),
        ),
        OptionValue::Target(t) => Json::String(t.name().into()),
        OptionValue::Module(m) => Json::String(m.name().into()),
        OptionValue::ModuleResolution(m) => Json::String(m.name().into()),
        OptionValue::ModuleDetection(m) => Json::String(m.name().into()),
        OptionValue::Jsx(j) => Json::String(j.name().into()),
        OptionValue::NewLine(n) => Json::String(n.name().into()),
        OptionValue::Paths(map) => Json::Object(
            map.0
                .iter()
                .map(|(k, subs)| {
                    (
                        k.clone(),
                        Json::Array(subs.iter().map(|s| Json::String(s.clone())).collect()),
                    )
                })
                .collect(),
        ),
        OptionValue::Plugins(_) => return None,
    })
}

/// Implied options tsc prints after the explicit ones, when their computed value differs
/// from the value computed for empty options.
fn implied_options(o: &CompilerOptions) -> Vec<(&'static str, Json)> {
    let e = o.effective();
    let mut out = Vec::new();
    let mut push_if = |id: OptionId, differs: bool, value: Json| {
        if !o.is_set(id) && differs {
            out.push((id.name(), value));
        }
    };
    push_if(
        OptionId::Module,
        e.module() != ModuleKind::ES2022,
        Json::String(e.module().name().into()),
    );
    push_if(
        OptionId::ModuleResolution,
        e.module_resolution() != ModuleResolution::Bundler,
        Json::String(e.module_resolution().name().into()),
    );
    push_if(
        OptionId::ModuleDetection,
        e.module_detection() != ModuleDetection::Auto,
        Json::String(e.module_detection().name().into()),
    );
    push_if(
        OptionId::IsolatedModules,
        e.isolated_modules(),
        Json::Bool(true),
    );
    push_if(
        OptionId::PreserveConstEnums,
        e.preserve_const_enums(),
        Json::Bool(true),
    );
    push_if(OptionId::Declaration, e.declaration(), Json::Bool(true));
    push_if(OptionId::Incremental, e.incremental(), Json::Bool(true));
    push_if(
        OptionId::UseDefineForClassFields,
        !e.use_define_for_class_fields(),
        Json::Bool(false),
    );
    push_if(
        OptionId::ResolveJsonModule,
        !e.resolve_json_module(),
        Json::Bool(false),
    );
    push_if(OptionId::AllowJs, e.allow_js(), Json::Bool(true));
    push_if(
        OptionId::AllowImportingTsExtensions,
        e.allow_importing_ts_extensions(),
        Json::Bool(true),
    );
    out
}

/// Builds the `--showConfig` JSON tree.
pub fn show_config_json(cfg: &ParsedConfig) -> Json {
    let mut opts = Vec::new();
    for (id, v) in cfg.options.iter() {
        if id.decl().flags & flags::HIDDEN != 0 {
            continue;
        }
        if let Some(j) = option_json(cfg, id, v) {
            opts.push((id.name().to_owned(), j));
        }
    }
    for (name, j) in implied_options(&cfg.options) {
        opts.push((name.to_owned(), j));
    }
    let mut top = vec![("compilerOptions".to_owned(), Json::Object(opts))];
    if !cfg.references.is_empty() {
        let refs = cfg
            .references
            .iter()
            .map(|r| {
                let mut o = vec![("path".to_owned(), Json::String(r.path.clone()))];
                if r.circular {
                    o.push(("circular".to_owned(), Json::Bool(true)));
                }
                Json::Object(o)
            })
            .collect();
        top.push(("references".to_owned(), Json::Array(refs)));
    }
    if !cfg.file_names.is_empty() {
        top.push((
            "files".to_owned(),
            Json::Array(
                cfg.file_names
                    .iter()
                    .map(|f| Json::String(rel(cfg, f)))
                    .collect(),
            ),
        ));
    }
    if let Some(inc) = &cfg.include {
        let specs = inc.display_specs(&cfg.config_dir, cfg.case_sensitive());
        if !specs.is_empty() {
            top.push((
                "include".to_owned(),
                Json::Array(specs.into_iter().map(Json::String).collect()),
            ));
        }
    }
    let exclude = cfg.exclude_display();
    if !exclude.is_empty() {
        top.push((
            "exclude".to_owned(),
            Json::Array(exclude.into_iter().map(Json::String).collect()),
        ));
    }
    Json::Object(top)
}

/// The exact text `tsc --showConfig` prints (without a trailing newline).
pub fn show_config(cfg: &ParsedConfig) -> String {
    show_config_json(cfg).pretty()
}
