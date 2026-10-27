//! `no-restricted-imports`: imports and re-exports of configured modules (`paths`, by exact
//! name) or of modules matching `patterns` (gitignore-style `group`s or a `regex`), optionally
//! only for some imported names.

use super::prelude::*;
use crate::config::glob::Glob;
use crate::regex_exec;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-restricted-imports",
        "Disallow specified modules when loaded by `import`",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoRestrictedImports>,
};

#[derive(Debug, Default)]
struct Restriction {
    message: Option<String>,
    import_names: Option<Vec<String>>,
    allow_import_names: Option<Vec<String>>,
    import_name_pattern: Option<String>,
    allow_import_name_pattern: Option<String>,
    allow_type_imports: bool,
}

#[derive(Debug)]
enum Matcher {
    /// node-ignore rules: (glob, negated, directories only).
    Ignore(Vec<(Glob, bool, bool)>, bool),
    Regex(String, bool),
}

impl Matcher {
    fn matches(&self, source: &str) -> bool {
        match self {
            Matcher::Regex(re, case) => {
                regex_exec::test(re, if *case { "u" } else { "iu" }, source)
            }
            Matcher::Ignore(rules, case) => {
                let path = if *case {
                    source.to_owned()
                } else {
                    source.to_lowercase()
                };
                let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                // A path is ignored when one of its parent directories is, else by the last
                // rule matching it (`!` rules un-ignore).
                for end in 1..=parts.len() {
                    let prefix = parts[..end].join("/");
                    let is_dir = end < parts.len();
                    let mut ignored = false;
                    for (glob, negated, dir_only) in rules {
                        if (*dir_only && !is_dir) || ignored != *negated {
                            continue;
                        }
                        if glob.matches(&prefix) {
                            ignored = !negated;
                        }
                    }
                    if ignored {
                        return true;
                    }
                }
                false
            }
        }
    }
}

fn ignore_rules(group: &[String], case: bool) -> Vec<(Glob, bool, bool)> {
    let mut out = Vec::new();
    for line in group {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, p) = match line.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, line.strip_prefix('\\').unwrap_or(line)),
        };
        let p = if case { p.to_owned() } else { p.to_lowercase() };
        let dir_only = p.ends_with('/');
        let body = p.trim_end_matches('/');
        // Without an inner `/` the pattern matches at any depth.
        let anchored = body.contains('/');
        let pat = if anchored {
            body.to_owned()
        } else {
            format!("**/{body}")
        };
        out.push((Glob::new(&pat), negated, dir_only));
    }
    out
}

#[derive(Debug, Default)]
struct NoRestrictedImports {
    paths: Vec<(String, Restriction)>,
    patterns: Vec<(Matcher, Restriction)>,
}

fn opt_list(o: &serde_json::Map<String, Value>, k: &str) -> Option<Vec<String>> {
    o.get(k).and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect()
    })
}

fn restriction(o: &serde_json::Map<String, Value>) -> Restriction {
    Restriction {
        message: str_field(o, "message").map(String::from),
        import_names: opt_list(o, "importNames"),
        allow_import_names: opt_list(o, "allowImportNames"),
        import_name_pattern: str_field(o, "importNamePattern").map(String::from),
        allow_import_name_pattern: str_field(o, "allowImportNamePattern").map(String::from),
        allow_type_imports: bool_field(o, "allowTypeImports", false),
    }
}

impl FromOptions for NoRestrictedImports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut out = Self::default();
        let wrapped = opts
            .first()
            .and_then(Value::as_object)
            .filter(|o| o.contains_key("paths") || o.contains_key("patterns"));
        let paths: &[Value] = match wrapped {
            Some(o) => o.get("paths").and_then(Value::as_array).map_or(&[], |a| a),
            None => opts,
        };
        for p in paths {
            match p {
                Value::String(s) => out.paths.push((s.clone(), Restriction::default())),
                Value::Object(o) => {
                    let name = str_field(o, "name").unwrap_or_default().to_owned();
                    out.paths.push((name, restriction(o)));
                }
                _ => {}
            }
        }
        let patterns: &[Value] = wrapped
            .and_then(|o| o.get("patterns"))
            .and_then(Value::as_array)
            .map_or(&[], |a| a);
        if patterns.first().is_some_and(Value::is_string) {
            let group: Vec<String> = patterns
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            out.patterns.push((
                Matcher::Ignore(ignore_rules(&group, false), false),
                Restriction::default(),
            ));
        } else {
            for p in patterns.iter().filter_map(Value::as_object) {
                let case = bool_field(p, "caseSensitive", false);
                let matcher = if let Some(re) = str_field(p, "regex") {
                    Matcher::Regex(re.to_owned(), case)
                } else if let Some(group) = opt_list(p, "group") {
                    Matcher::Ignore(ignore_rules(&group, case), case)
                } else {
                    continue;
                };
                out.patterns.push((matcher, restriction(p)));
            }
        }
        Ok(out)
    }
}

/// `'a'`, `'a' and 'b'`, `'a', 'b', and 'c'` (`Intl.ListFormat` en-US).
fn format_names(names: &[String]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
    match q.len() {
        0 => String::new(),
        1 => q[0].clone(),
        2 => format!("{} and {}", q[0], q[1]),
        n => format!("{}, and {}", q[..n - 1].join(", "), q[n - 1]),
    }
}

fn is_or_are(names: &[String]) -> &'static str {
    if names.len() == 1 { "is" } else { "are" }
}

/// `RegExp.prototype.toString` for a `u` regex.
fn regex_text(src: &str) -> String {
    format!("/{}/u", if src.is_empty() { "(?:)" } else { src })
}

struct Named {
    name: String,
    span: Span,
    type_only: bool,
}

struct Import {
    node: NodeId,
    source: String,
    /// Grouped by name, in first-seen order.
    names: Vec<(String, Vec<Named>)>,
    whole_type_only: bool,
}

fn module_export_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => ident_name(ast, id).map(String::from),
    }
}

fn with_msg(base: &str, msg: &Option<String>) -> String {
    match msg {
        Some(m) => format!("{base} {m}"),
        None => base.to_owned(),
    }
}

fn id_for(base: &'static str, custom: &'static str, msg: &Option<String>) -> &'static str {
    if msg.is_some() { custom } else { base }
}

impl NoRestrictedImports {
    fn collect(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Import> {
        let ast = cx.ast();
        let string = |id: NodeId| match *ast.node(id) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            _ => None,
        };
        let mut names: Vec<(String, Vec<Named>)> = Vec::new();
        let mut add = |name: String, span: Span, type_only: bool| {
            let n = Named {
                name: name.clone(),
                span,
                type_only,
            };
            match names.iter_mut().find(|(k, _)| *k == name) {
                Some((_, v)) => v.push(n),
                None => names.push((name, vec![n])),
            }
        };
        let (source, whole_type_only, all_type) = match *ast.node(node) {
            Node::ImportDecl {
                type_only,
                specifiers,
                source,
                ..
            } => {
                let specs = ast.list(specifiers);
                let mut all = !specs.is_empty();
                for &s in specs {
                    let (name, ty) = match *ast.node(s) {
                        Node::ImportDefaultSpecifier { .. } => ("default".to_owned(), false),
                        Node::ImportNamespaceSpecifier { .. } => ("*".to_owned(), false),
                        Node::ImportSpecifier {
                            type_only,
                            imported,
                            local,
                        } => (
                            module_export_name(ast, imported.unwrap_or(local))?,
                            type_only,
                        ),
                        _ => continue,
                    };
                    all &= ty;
                    add(name, cx.span(s), ty);
                }
                (string(source)?.trim().to_owned(), type_only, all)
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source: Some(source),
                ..
            } => {
                let specs = ast.list(specifiers);
                let mut all = !specs.is_empty();
                for &s in specs {
                    let Node::ExportSpecifier {
                        type_only, local, ..
                    } = *ast.node(s)
                    else {
                        continue;
                    };
                    all &= type_only;
                    add(module_export_name(ast, local)?, cx.span(s), type_only);
                }
                (string(source)?.trim().to_owned(), type_only, all)
            }
            Node::ExportAll {
                type_only, source, ..
            } => {
                // ESLint takes the declaration's second token (`*`, or `type`).
                let start = cx.span(node).start as usize + "export".len();
                let pos = skip_trivia_forward(cx.src(), start) as u32;
                let len = if cx.src()[pos as usize..].starts_with('*') {
                    1
                } else {
                    4
                };
                add("*".to_owned(), Span::new(pos, pos + len), false);
                (string(source)?.trim().to_owned(), type_only, false)
            }
            Node::ImportEquals {
                type_only,
                module_ref,
                ..
            } => {
                let Node::ExternalModuleRef { expr } = *ast.node(module_ref) else {
                    return None;
                };
                (string(expr)?, type_only, false)
            }
            _ => return None,
        };
        Some(Import {
            node,
            source,
            names,
            whole_type_only: whole_type_only || all_type,
        })
    }

    fn check_path(cx: &mut Ctx<'_, '_>, imp: &Import, r: &Restriction) {
        let src = &imp.source;
        let msg = &r.message;
        if r.allow_type_imports && imp.whole_type_only {
            return;
        }
        if r.import_names.is_none() && r.allow_import_names.is_none() {
            let text = with_msg(
                &format!("'{src}' import is restricted from being used."),
                msg,
            );
            cx.report_node(imp.node, text)
                .id(id_for("path", "pathWithCustomMessage", msg));
            return;
        }
        for (name, specs) in &imp.names {
            if name == "*" {
                let span = specs[0].span;
                if let Some(names) = &r.import_names {
                    let text = format!(
                        "* import is invalid because {} from '{src}' {} restricted.",
                        format_names(names),
                        is_or_are(names)
                    );
                    cx.report(span, with_msg(&text, msg)).id(id_for(
                        "everything",
                        "everythingWithCustomMessage",
                        msg,
                    ));
                } else if let Some(names) = &r.allow_import_names {
                    let text = format!(
                        "* import is invalid because only {} from '{src}' {} allowed.",
                        format_names(names),
                        is_or_are(names)
                    );
                    cx.report(span, with_msg(&text, msg)).id(id_for(
                        "everythingWithAllowImportNames",
                        "everythingWithAllowImportNamesAndCustomMessage",
                        msg,
                    ));
                }
                continue;
            }
            for s in specs {
                if r.allow_type_imports && s.type_only {
                    continue;
                }
                if r.import_names.as_ref().is_some_and(|n| n.contains(name)) {
                    let text = format!("'{}' import from '{src}' is restricted.", s.name);
                    cx.report(s.span, with_msg(&text, msg)).id(id_for(
                        "importName",
                        "importNameWithCustomMessage",
                        msg,
                    ));
                }
                if let Some(allowed) = &r.allow_import_names
                    && !allowed.contains(name)
                {
                    let text = format!(
                        "'{name}' import from '{src}' is restricted because only {} {} allowed.",
                        format_names(allowed),
                        is_or_are(allowed)
                    );
                    cx.report(s.span, with_msg(&text, msg)).id(id_for(
                        "allowedImportName",
                        "allowedImportNameWithCustomMessage",
                        msg,
                    ));
                }
            }
        }
    }

    fn check_pattern(cx: &mut Ctx<'_, '_>, imp: &Import, r: &Restriction) {
        let src = &imp.source;
        let msg = &r.message;
        if r.allow_type_imports && imp.whole_type_only {
            return;
        }
        let names = &r.import_names;
        let allowed = &r.allow_import_names;
        let pattern = &r.import_name_pattern;
        let allow_pattern = &r.allow_import_name_pattern;
        if names.is_none() && allowed.is_none() && pattern.is_none() && allow_pattern.is_none() {
            let text = format!("'{src}' import is restricted from being used by a pattern.");
            cx.report_node(imp.node, with_msg(&text, msg)).id(id_for(
                "patterns",
                "patternWithCustomMessage",
                msg,
            ));
            return;
        }
        for (name, specs) in &imp.names {
            if name == "*" {
                let span = specs[0].span;
                let (text, id) = if let Some(n) = names {
                    (
                        format!(
                            "* import is invalid because {} from '{src}' {} restricted from being used by a pattern.",
                            format_names(n),
                            is_or_are(n)
                        ),
                        id_for(
                            "patternAndEverything",
                            "patternAndEverythingWithCustomMessage",
                            msg,
                        ),
                    )
                } else if let Some(a) = allowed {
                    (
                        format!(
                            "* import is invalid because only {} from '{src}' {} allowed.",
                            format_names(a),
                            is_or_are(a)
                        ),
                        id_for(
                            "everythingWithAllowImportNames",
                            "everythingWithAllowImportNamesAndCustomMessage",
                            msg,
                        ),
                    )
                } else if let Some(p) = allow_pattern {
                    (
                        format!(
                            "* import is invalid because only imports that match the pattern '{}' from '{src}' are allowed.",
                            regex_text(p)
                        ),
                        id_for(
                            "everythingWithAllowedImportNamePattern",
                            "everythingWithAllowedImportNamePatternWithCustomMessage",
                            msg,
                        ),
                    )
                } else {
                    (
                        format!(
                            "* import is invalid because import name matching '{}' pattern from '{src}' is restricted from being used.",
                            regex_text(pattern.as_deref().unwrap_or_default())
                        ),
                        id_for(
                            "patternAndEverythingWithRegexImportName",
                            "patternAndEverythingWithRegexImportNameAndCustomMessage",
                            msg,
                        ),
                    )
                };
                cx.report(span, with_msg(&text, msg)).id(id);
                continue;
            }
            let restricted = names.as_ref().is_some_and(|n| n.contains(name))
                || pattern
                    .as_ref()
                    .is_some_and(|p| regex_exec::test(p, "u", name));
            let not_allowed = allowed.as_ref().map(|a| !a.contains(name));
            let not_matching = allow_pattern
                .as_ref()
                .is_some_and(|p| !regex_exec::test(p, "u", name));
            for s in specs {
                if r.allow_type_imports && s.type_only {
                    continue;
                }
                if restricted {
                    let text = format!(
                        "'{name}' import from '{src}' is restricted from being used by a pattern."
                    );
                    cx.report(s.span, with_msg(&text, msg)).id(id_for(
                        "patternAndImportName",
                        "patternAndImportNameWithCustomMessage",
                        msg,
                    ));
                }
                if let (Some(true), Some(a)) = (not_allowed, allowed) {
                    let text = format!(
                        "'{name}' import from '{src}' is restricted because only {} {} allowed.",
                        format_names(a),
                        is_or_are(a)
                    );
                    cx.report(s.span, with_msg(&text, msg)).id(id_for(
                        "allowedImportName",
                        "allowedImportNameWithCustomMessage",
                        msg,
                    ));
                } else if not_allowed.is_none() && not_matching {
                    let p = regex_text(allow_pattern.as_deref().unwrap_or_default());
                    let text = format!(
                        "'{name}' import from '{src}' is restricted because only imports that match the pattern '{p}' are allowed from '{src}'."
                    );
                    cx.report(s.span, with_msg(&text, msg)).id(id_for(
                        "allowedImportNamePattern",
                        "allowedImportNamePatternWithCustomMessage",
                        msg,
                    ));
                }
            }
        }
    }
}

impl Rule for NoRestrictedImports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportDecl,
            NodeKind::ExportNamed,
            NodeKind::ExportAll,
            NodeKind::ImportEquals,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.paths.is_empty() && self.patterns.is_empty() {
            return;
        }
        let Some(imp) = Self::collect(cx, node) else {
            return;
        };
        for (name, r) in &self.paths {
            if *name == imp.source {
                Self::check_path(cx, &imp, r);
            }
        }
        for (m, r) in &self.patterns {
            if m.matches(&imp.source) {
                Self::check_pattern(cx, &imp, r);
            }
        }
    }
}
