//! `--init`: writes tsc 7's starter `tsconfig.json` to the working directory.
//!
//! Observed from tsc 7's `Initialized-TSConfig-*` baselines: the template below, where an
//! option given on the command line replaces its template line (uncommented, with the given
//! value), `lib` goes right after the `types` line, and other options follow the template's
//! last option after a blank line, in command-line order; command-line
//! options (`--init` itself, `--watch`) are left out. File names are ignored. An
//! existing `tsconfig.json` is TS5054 (exit status 0, nothing written).

use ts_r_config::cli::{FLAGS, FlagTarget, Section};
use ts_r_config::options::{OptionType, flags as option_flags};
use ts_r_config::{FileSystem, OptionId, OptionValue, jsonc, path};

/// tsc 7's starter configuration. A `None` key is a fixed line; `Some(name)` a line an
/// option given on the command line replaces.
const TEMPLATE: &[(Option<&str>, &str)] = &[
    (None, "{"),
    (
        None,
        "  // Visit https://aka.ms/tsconfig to read more about this file",
    ),
    (None, "  \"compilerOptions\": {"),
    (None, "    // File Layout"),
    (Some("rootDir"), "    // \"rootDir\": \"./src\","),
    (Some("outDir"), "    // \"outDir\": \"./dist\","),
    (None, ""),
    (None, "    // Environment Settings"),
    (None, "    // See also https://aka.ms/tsconfig/module"),
    (Some("module"), "    \"module\": \"nodenext\","),
    (Some("target"), "    \"target\": \"esnext\","),
    (Some("types"), "    \"types\": [],"),
    (Some("lib"), ""),
    (None, "    // For nodejs:"),
    (None, "    // \"lib\": [\"esnext\"],"),
    (None, "    // \"types\": [\"node\"],"),
    (None, "    // and npm install -D @types/node"),
    (None, ""),
    (None, "    // Other Outputs"),
    (Some("sourceMap"), "    \"sourceMap\": true,"),
    (Some("declaration"), "    \"declaration\": true,"),
    (Some("declarationMap"), "    \"declarationMap\": true,"),
    (None, ""),
    (None, "    // Stricter Typechecking Options"),
    (
        Some("noUncheckedIndexedAccess"),
        "    \"noUncheckedIndexedAccess\": true,",
    ),
    (
        Some("exactOptionalPropertyTypes"),
        "    \"exactOptionalPropertyTypes\": true,",
    ),
    (None, ""),
    (None, "    // Style Options"),
    (
        Some("noImplicitReturns"),
        "    // \"noImplicitReturns\": true,",
    ),
    (
        Some("noImplicitOverride"),
        "    // \"noImplicitOverride\": true,",
    ),
    (Some("noUnusedLocals"), "    // \"noUnusedLocals\": true,"),
    (
        Some("noUnusedParameters"),
        "    // \"noUnusedParameters\": true,",
    ),
    (
        Some("noFallthroughCasesInSwitch"),
        "    // \"noFallthroughCasesInSwitch\": true,",
    ),
    (
        Some("noPropertyAccessFromIndexSignature"),
        "    // \"noPropertyAccessFromIndexSignature\": true,",
    ),
    (None, ""),
    (None, "    // Recommended Options"),
    (Some("strict"), "    \"strict\": true,"),
    (Some("jsx"), "    \"jsx\": \"react-jsx\","),
    (
        Some("verbatimModuleSyntax"),
        "    \"verbatimModuleSyntax\": true,",
    ),
    (Some("isolatedModules"), "    \"isolatedModules\": true,"),
    (
        Some("noUncheckedSideEffectImports"),
        "    \"noUncheckedSideEffectImports\": true,",
    ),
    (
        Some("moduleDetection"),
        "    \"moduleDetection\": \"force\",",
    ),
    (Some("skipLibCheck"), "    \"skipLibCheck\": true,"),
];

/// What `--init` printed or failed with.
pub enum Outcome {
    /// The file was written; print this.
    Created(&'static str),
    /// TS5054: a `tsconfig.json` is already there (its path).
    Exists(String),
    /// The file could not be written (the OS error).
    WriteFailed(String),
}

/// tsc 7's message after writing the file.
const CREATED: &str =
    "\nCreated a new tsconfig.json\n\nYou can learn more at https://aka.ms/tsconfig\n";

/// Runs `--init` in `cwd` with the options given on the command line.
pub fn run(
    options: &ts_r_config::CompilerOptions,
    given: &[OptionId],
    fs: &dyn FileSystem,
    cwd: &str,
) -> Outcome {
    let target = path::combine(cwd, "tsconfig.json");
    if fs.file_exists(&target) {
        return Outcome::Exists(target);
    }
    let text = template(options, given, cwd, fs.case_sensitive());
    match std::fs::write(&target, text) {
        Ok(()) => Outcome::Created(CREATED),
        Err(e) => Outcome::WriteFailed(format!("'{target}': {e}")),
    }
}

/// The `tsconfig.json` text for the given options.
pub fn template(
    options: &ts_r_config::CompilerOptions,
    given: &[OptionId],
    cwd: &str,
    case_sensitive: bool,
) -> String {
    let mut seen: Vec<OptionId> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for &(key, line) in TEMPLATE {
        let Some(key) = key else {
            lines.push(line.to_owned());
            continue;
        };
        let value = given
            .iter()
            .copied()
            .find(|id| id.decl().name == key)
            .and_then(|id| {
                seen.push(id);
                options.get(id).map(|v| (id, v))
            });
        match value {
            Some((id, v)) => lines.push(entry(id, v, cwd, case_sensitive)),
            // `lib` has no line of its own in the template.
            None if line.is_empty() => {}
            None => lines.push(line.to_owned()),
        }
    }
    let mut extra = Vec::new();
    for &id in given {
        if seen.contains(&id) || command_line_only(id) {
            continue;
        }
        seen.push(id);
        if let Some(v) = options.get(id) {
            extra.push(entry(id, v, cwd, case_sensitive));
        }
    }
    if !extra.is_empty() {
        lines.push(String::new());
        lines.extend(extra);
    }
    let mut out = lines.join("\n");
    out.push_str("\n  }\n}\n");
    out
}

/// Options of tsc's "Command-line Options" section (`--init`, `--version`, `--watch`, ...)
/// and command-line-only ones are not written.
fn command_line_only(id: OptionId) -> bool {
    id.decl().flags & option_flags::CMDLINE_ONLY != 0
        || FLAGS
            .iter()
            .any(|f| f.target == FlagTarget::Compiler(id) && f.section == Section::CommandLine)
}

/// One `    "name": value,` line.
fn entry(id: OptionId, v: &OptionValue, cwd: &str, case_sensitive: bool) -> String {
    let mut out = String::from("    ");
    jsonc::write_json_string(id.decl().name, &mut out);
    out.push_str(": ");
    value(id, v, cwd, case_sensitive, &mut out);
    out.push(',');
    out
}

fn value(id: OptionId, v: &OptionValue, cwd: &str, case_sensitive: bool, out: &mut String) {
    let ty = id.decl().ty;
    let rel = |p: &str| {
        let r = path::relative(cwd, p, case_sensitive);
        if r.is_empty() { ".".to_owned() } else { r }
    };
    match v {
        OptionValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        OptionValue::Number(n) => out.push_str(&jsonc::format_number(*n)),
        OptionValue::String(s) if ty == OptionType::Path => jsonc::write_json_string(&rel(s), out),
        OptionValue::String(s) => jsonc::write_json_string(s, out),
        OptionValue::List(items) => {
            out.push('[');
            for (i, s) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if ty == OptionType::PathList {
                    jsonc::write_json_string(&rel(s), out);
                } else {
                    jsonc::write_json_string(s, out);
                }
            }
            out.push(']');
        }
        OptionValue::Target(t) => jsonc::write_json_string(t.name(), out),
        OptionValue::Module(m) => jsonc::write_json_string(m.name(), out),
        OptionValue::ModuleResolution(m) => jsonc::write_json_string(m.name(), out),
        OptionValue::ModuleDetection(m) => jsonc::write_json_string(m.name(), out),
        OptionValue::Jsx(j) => jsonc::write_json_string(j.name(), out),
        OptionValue::NewLine(n) => jsonc::write_json_string(n.name(), out),
        OptionValue::Paths(map) => {
            out.push('{');
            for (i, (k, subs)) in map.0.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                jsonc::write_json_string(k, out);
                out.push_str(": [");
                for (j, s) in subs.iter().enumerate() {
                    if j > 0 {
                        out.push_str(", ");
                    }
                    jsonc::write_json_string(s, out);
                }
                out.push(']');
            }
            out.push('}');
        }
        OptionValue::Plugins(_) => out.push_str("[]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_config::cli::parse_command_line;

    fn init(args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let cl = parse_command_line(&args, "/p", &ts_r_config::MemoryFs::default());
        assert!(cl.diagnostics.is_empty());
        template(&cl.options, &cl.given, "/p", true)
    }

    #[test]
    fn default_template() {
        let t = init(&["--init"]);
        assert!(t.starts_with("{\n  // Visit https://aka.ms/tsconfig"));
        assert!(t.contains("    \"types\": [],\n    // For nodejs:\n"));
        assert!(t.ends_with("    \"skipLibCheck\": true,\n  }\n}\n"));
    }

    #[test]
    fn given_options_replace_or_follow() {
        let t = init(&[
            "--init",
            "--target",
            "es5",
            "--noUnusedLocals",
            "--lib",
            "es5,es2015.core",
            "--declarationDir",
            "lib",
            "--skipLibCheck",
            "--types",
            "jquery,mocha",
        ]);
        assert!(t.contains("    \"target\": \"es5\",\n"));
        assert!(t.contains("    \"noUnusedLocals\": true,\n"));
        assert!(t.contains(
            "    \"types\": [\"jquery\", \"mocha\"],\n    \"lib\": [\"es5\", \"es2015.core\"],\n    // For nodejs:\n"
        ));
        assert!(t.ends_with(
            "    \"skipLibCheck\": true,\n\n    \"declarationDir\": \"lib\",\n  }\n}\n"
        ));
    }
}
